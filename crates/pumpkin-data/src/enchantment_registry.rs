//! Runtime (plugin-defined) enchantments, appended after the vanilla ones.
//!
//! Vanilla enchantments are generated at compile time with ids `0..Enchantment::ALL.len()`.
//! Runtime enchantments take the next ids in registration order and are leaked to
//! `&'static Enchantment`, so they work everywhere a vanilla one does. The generated
//! `Enchantment::from_name`, `from_id` and `has_tag` fall back to this registry.
//!
//! Clients receive the enchantment registry once, during configuration, and Pumpkin can't
//! reconfigure them afterwards. So the registry is [frozen](freeze) as soon as it has been
//! sent to a client. After that, re-registering an existing enchantment still works (for
//! plugin hot reload), but adding a new one is refused until the server restarts.
//!
//! Worlds load before plugins, so a saved item may mention an enchantment that hasn't been
//! registered yet. Rather than dropping the item, [`placeholder`] reserves an id for the
//! name. When the plugin later registers it, the real definition takes over that id.

use std::sync::{
    RwLock,
    atomic::{AtomicBool, Ordering},
};

use crate::enchantment::{AttributeModifierSlot, Cost, Enchantment, EnchantmentEffects};
use crate::item::Item;
use crate::tag::{RegistryKey, Tag, get_latest_map};

/// A runtime enchantment definition, converted to an [`Enchantment`] on registration.
#[derive(Clone, Debug)]
pub struct EnchantmentDefinition {
    /// Namespaced id, e.g. `my_plugin:lifesteal`.
    pub name: String,
    /// Plain-text name, used where Pumpkin expects a translation key.
    pub display_name: String,
    /// An item tag (`#minecraft:pickaxes`) or a single item (`minecraft:diamond_pickaxe`).
    pub supported_items: String,
    pub max_level: i32,
    pub weight: i32,
    pub anvil_cost: u32,
    pub slots: Vec<AttributeModifierSlot>,
    pub min_cost: Cost,
    pub max_cost: Cost,
    /// Ids of enchantments this one can't be combined with.
    pub exclusive_set: Vec<String>,
    /// Enchantment tags it belongs to, e.g. `minecraft:in_enchanting_table`.
    pub tags: Vec<String>,
}

/// A registered runtime enchantment.
#[derive(Clone)]
pub struct DynamicEnchantment {
    pub enchantment: &'static Enchantment,
    /// The `supported_items` value as given, for syncing to clients.
    pub supported_items: String,
    /// Reserved for a name found on a saved item, not yet registered by anyone.
    pub placeholder: bool,
    tags: Vec<&'static Tag>,
}

/// Placeholders accept anything so items keep working until the real definition arrives.
const PLACEHOLDER_SUPPORTED_ITEMS: &str = "#minecraft:enchantable/vanishing";

const NO_EFFECTS: EnchantmentEffects = EnchantmentEffects {
    projectile_spawned: &[],
    post_attack: &[],
    projectile_count: &[],
    projectile_spread: &[],
    projectile_piercing: &[],
    ammo_use: &[],
    damage: &[],
    knockback: &[],
    armor_effectiveness: &[],
    damage_protection: &[],
    hit_block: &[],
    item_damage: &[],
    equipment_drops: &[],
    fishing_time_reduction: &[],
    fishing_luck_bonus: &[],
    block_experience: &[],
    mob_experience: &[],
    repair_with_xp: &[],
    smash_damage_per_fallen_block: &[],
    trident_return_acceleration: &[],
    trident_spin_attack_strength: None,
    crossbow_charge_time: None,
    location_changed: &[],
    prevent_armor_change: false,
    prevent_equipment_drop: false,
};

/// Indexed by `id - Enchantment::ALL.len()`.
static ENTRIES: RwLock<Vec<DynamicEnchantment>> = RwLock::new(Vec::new());
static FROZEN: AtomicBool = AtomicBool::new(false);

fn vanilla_count() -> usize {
    Enchantment::ALL.len()
}

fn read() -> std::sync::RwLockReadGuard<'static, Vec<DynamicEnchantment>> {
    ENTRIES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write() -> std::sync::RwLockWriteGuard<'static, Vec<DynamicEnchantment>> {
    ENTRIES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// Stops new enchantments from being added. Called when the registry is first sent to a
/// client, since clients can't be told about entries added later.
///
/// Takes the write lock so a registration already in progress finishes first, and is
/// included in what's sent.
pub fn freeze() {
    let _entries = write();
    FROZEN.store(true, Ordering::Relaxed);
}

#[must_use]
pub fn is_frozen() -> bool {
    FROZEN.load(Ordering::Relaxed)
}

/// Registers a runtime enchantment, or returns the existing one with the same name.
///
/// # Errors
/// If the name is invalid or vanilla, a referenced tag or item doesn't exist, the registry
/// is full, or the registry is frozen and the enchantment is new.
pub fn register(definition: EnchantmentDefinition) -> Result<&'static Enchantment, String> {
    let name = definition.name.as_str();
    validate_name(name)?;
    if let Some(existing) = find(name).filter(|e| !e.placeholder) {
        return Ok(existing.enchantment);
    }

    // Resolve everything before taking the write lock: these lookups go through
    // `Enchantment::from_name`, which reads this registry.
    let supported_items = resolve_items(&definition.supported_items)?;
    let tags = definition
        .tags
        .iter()
        .map(|tag| resolve_enchantment_tag(tag))
        .collect::<Result<Vec<_>, _>>()?;
    let exclusive_set = resolve_exclusive_set(&definition.exclusive_set)?;

    let mut entries = write();
    let index = match entries.iter().position(|e| e.enchantment.name == name) {
        Some(index) if !entries[index].placeholder => return Ok(entries[index].enchantment),
        Some(index) => index,
        None if is_frozen() => {
            return Err(format!(
                "Enchantment '{name}' can't be added after players have joined; restart the server to use it"
            ));
        }
        None => entries.len(),
    };
    let id = u8::try_from(vanilla_count() + index)
        .map_err(|_| "Too many enchantments: ids are limited to 255".to_string())?;

    let enchantment: &'static Enchantment = Box::leak(Box::new(Enchantment {
        id,
        name: leak_str(name),
        registry_key: leak_str(name),
        description: leak_str(&definition.display_name),
        anvil_cost: definition.anvil_cost,
        supported_items,
        primary_items: None,
        exclusive_set,
        max_level: definition.max_level,
        slots: Box::leak(definition.slots.into_boxed_slice()),
        weight: definition.weight,
        min_cost: definition.min_cost,
        max_cost: definition.max_cost,
        effects: NO_EFFECTS,
    }));
    let entry = DynamicEnchantment {
        enchantment,
        supported_items: definition.supported_items,
        placeholder: false,
        tags,
    };
    if index < entries.len() {
        entries[index] = entry;
    } else {
        entries.push(entry);
    }
    Ok(enchantment)
}

/// Reserves an id for an enchantment found on a saved item but not registered yet, so the
/// item isn't dropped. Returns `None` once the registry is frozen, for vanilla-namespaced or
/// malformed names, or when the registry is full.
#[must_use]
pub fn placeholder(name: &str) -> Option<&'static Enchantment> {
    if is_frozen() || validate_name(name).is_err() {
        return None;
    }
    if let Some(existing) = find(name) {
        return Some(existing.enchantment);
    }
    let supported_items = resolve_items(PLACEHOLDER_SUPPORTED_ITEMS).ok()?;

    let mut entries = write();
    if let Some(existing) = entries.iter().find(|e| e.enchantment.name == name) {
        return Some(existing.enchantment);
    }
    if is_frozen() {
        return None;
    }
    let id = u8::try_from(vanilla_count() + entries.len()).ok()?;
    let enchantment: &'static Enchantment = Box::leak(Box::new(Enchantment {
        id,
        name: leak_str(name),
        registry_key: leak_str(name),
        description: leak_str(name),
        anvil_cost: 1,
        supported_items,
        primary_items: None,
        exclusive_set: None,
        max_level: 255,
        slots: &[AttributeModifierSlot::Any],
        weight: 1,
        min_cost: Cost {
            base: 1,
            per_level_above_first: 0,
        },
        max_cost: Cost {
            base: 1,
            per_level_above_first: 0,
        },
        effects: NO_EFFECTS,
    }));
    entries.push(DynamicEnchantment {
        enchantment,
        supported_items: PLACEHOLDER_SUPPORTED_ITEMS.to_string(),
        placeholder: true,
        tags: Vec::new(),
    });
    Some(enchantment)
}

#[must_use]
pub fn from_name(name: &str) -> Option<&'static Enchantment> {
    find(name).map(|e| e.enchantment)
}

#[must_use]
pub fn from_id(id: u8) -> Option<&'static Enchantment> {
    let index = usize::from(id).checked_sub(vanilla_count())?;
    read().get(index).map(|e| e.enchantment)
}

/// All runtime enchantments, in id order.
#[must_use]
pub fn dynamic() -> Vec<DynamicEnchantment> {
    read().clone()
}

/// Vanilla and runtime enchantments, in id order.
#[must_use]
pub fn all() -> Vec<&'static Enchantment> {
    Enchantment::ALL
        .iter()
        .copied()
        .chain(read().iter().map(|e| e.enchantment))
        .collect()
}

/// Every enchantment in `tag`, vanilla and runtime, in id order.
#[must_use]
pub fn tagged(tag: &'static Tag) -> Vec<&'static Enchantment> {
    tag.1
        .iter()
        .filter_map(|&id| u8::try_from(id).ok().and_then(Enchantment::from_id))
        .chain(
            read()
                .iter()
                .filter(|e| e.tags.iter().any(|t| same_tag(t, tag)))
                .map(|e| e.enchantment),
        )
        .collect()
}

/// Whether a runtime enchantment belongs to `tag`. Always `false` for vanilla ones, whose
/// membership is in the static tag data.
#[must_use]
pub fn has_tag(enchantment: &Enchantment, tag: &Tag) -> bool {
    let Some(index) = usize::from(enchantment.id).checked_sub(vanilla_count()) else {
        return false;
    };
    read()
        .get(index)
        .is_some_and(|e| e.tags.iter().any(|t| same_tag(t, tag)))
}

fn find(name: &str) -> Option<DynamicEnchantment> {
    read().iter().find(|e| e.enchantment.name == name).cloned()
}

/// Tag constants are promoted per use site, so compare contents rather than addresses.
fn same_tag(a: &Tag, b: &Tag) -> bool {
    std::ptr::eq(a, b) || a.0 == b.0
}

fn validate_name(name: &str) -> Result<(), String> {
    let Some((namespace, path)) = name.split_once(':') else {
        return Err(format!(
            "Enchantment id '{name}' needs a namespace, e.g. my_plugin:{name}"
        ));
    };
    if namespace.is_empty() || path.is_empty() {
        return Err(format!("Invalid enchantment id '{name}'"));
    }
    if namespace == "minecraft" {
        return Err(format!(
            "Enchantment id '{name}' uses the minecraft namespace, which is reserved for vanilla"
        ));
    }
    Ok(())
}

fn resolve_items(items: &str) -> Result<&'static Tag, String> {
    if let Some(tag) = items.strip_prefix('#') {
        let map = get_latest_map(RegistryKey::Item);
        return map
            .get(tag)
            .or_else(|| map.get(format!("minecraft:{tag}").as_str()))
            .copied()
            .ok_or_else(|| format!("Unknown item tag '{items}'"));
    }
    let key = items.strip_prefix("minecraft:").unwrap_or(items);
    let item = Item::from_registry_key(key).ok_or_else(|| format!("Unknown item '{items}'"))?;
    let names: &'static [&'static str] = Box::leak(Box::new([leak_str(key)]));
    let ids: &'static [u16] = Box::leak(Box::new([item.id]));
    Ok(Box::leak(Box::new((names, ids))))
}

fn resolve_enchantment_tag(tag: &str) -> Result<&'static Tag, String> {
    let tag = tag.strip_prefix('#').unwrap_or(tag);
    let map = get_latest_map(RegistryKey::Enchantment);
    map.get(tag)
        .or_else(|| map.get(format!("minecraft:{tag}").as_str()))
        .copied()
        .ok_or_else(|| format!("Unknown enchantment tag '#{tag}'"))
}

fn resolve_exclusive_set(names: &[String]) -> Result<Option<&'static Tag>, String> {
    if names.is_empty() {
        return Ok(None);
    }
    let mut keys = Vec::with_capacity(names.len());
    let mut ids = Vec::with_capacity(names.len());
    for name in names {
        let enchantment = Enchantment::from_name(name)
            .or_else(|| Enchantment::from_name(&format!("minecraft:{name}")))
            .ok_or_else(|| format!("Unknown enchantment '{name}' in exclusive set"))?;
        keys.push(enchantment.registry_key);
        ids.push(u16::from(enchantment.id));
    }
    let keys: &'static [&'static str] = Box::leak(keys.into_boxed_slice());
    let ids: &'static [u16] = Box::leak(ids.into_boxed_slice());
    Ok(Some(Box::leak(Box::new((keys, ids)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_component_impl::EnchantmentsImpl;
    use crate::item_stack::ItemStack;
    use crate::tag::{Enchantment as EnchantmentTag, Taggable};
    use pumpkin_nbt::compound::NbtCompound;
    use pumpkin_nbt::tag::NbtTag;

    // The registry is global and never shrinks, so every test uses its own names and none
    // of them freezes it (see tests/enchantment_registry_frozen.rs for that).

    fn definition(name: &str, tags: &[&str]) -> EnchantmentDefinition {
        EnchantmentDefinition {
            name: name.to_string(),
            display_name: "Test".to_string(),
            supported_items: "#minecraft:pickaxes".to_string(),
            max_level: 2,
            weight: 2,
            anvil_cost: 4,
            slots: vec![AttributeModifierSlot::MainHand],
            min_cost: Cost {
                base: 15,
                per_level_above_first: 9,
            },
            max_cost: Cost {
                base: 65,
                per_level_above_first: 9,
            },
            exclusive_set: vec!["minecraft:silk_touch".to_string()],
            tags: tags.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn registered_enchantments_resolve_like_vanilla_ones() {
        let enchantment = register(definition(
            "test:resolves",
            &["minecraft:in_enchanting_table", "minecraft:tradeable"],
        ))
        .unwrap();

        assert!(usize::from(enchantment.id) >= Enchantment::ALL.len());
        assert!(Enchantment::from_name("test:resolves") == Some(enchantment));
        assert!(Enchantment::from_id(enchantment.id) == Some(enchantment));
        assert!(enchantment.can_enchant(&Item::DIAMOND_PICKAXE));
        assert!(!enchantment.can_enchant(&Item::DIAMOND_AXE));
        assert!(!Enchantment::are_compatible(
            enchantment,
            &Enchantment::SILK_TOUCH
        ));
        assert!(all().contains(&enchantment));
    }

    #[test]
    fn tags_cover_runtime_enchantments() {
        let tagged_one = register(definition(
            "test:tagged",
            &["minecraft:in_enchanting_table", "minecraft:tradeable"],
        ))
        .unwrap();
        let untagged = register(definition("test:untagged", &[])).unwrap();

        assert!(tagged_one.has_tag(&EnchantmentTag::MINECRAFT_IN_ENCHANTING_TABLE));
        assert!(tagged_one.has_tag(&EnchantmentTag::MINECRAFT_TRADEABLE));
        assert!(!tagged_one.has_tag(&EnchantmentTag::MINECRAFT_CURSE));
        assert!(!untagged.has_tag(&EnchantmentTag::MINECRAFT_TRADEABLE));
        // Vanilla membership is unchanged.
        assert!(Enchantment::SHARPNESS.has_tag(&EnchantmentTag::MINECRAFT_TRADEABLE));
        assert!(!Enchantment::MENDING.has_tag(&EnchantmentTag::MINECRAFT_IN_ENCHANTING_TABLE));

        let tradeable = tagged(&EnchantmentTag::MINECRAFT_TRADEABLE);
        assert!(tradeable.contains(&tagged_one));
        assert!(!tradeable.contains(&untagged));
        assert!(tradeable.contains(&&Enchantment::SHARPNESS));
    }

    #[test]
    fn registering_again_returns_the_same_enchantment() {
        let first = register(definition("test:again", &[])).unwrap();
        let mut changed = definition("test:again", &[]);
        changed.max_level = 5;
        let second = register(changed).unwrap();
        assert!(std::ptr::eq(first, second));
        assert_eq!(second.max_level, 2);
    }

    #[test]
    fn rejects_bad_definitions() {
        assert!(register(definition("no_namespace", &[])).is_err());
        assert!(register(definition("minecraft:new_thing", &[])).is_err());
        assert!(register(definition("test:bad_tag", &["minecraft:no_such_tag"])).is_err());
        let mut bad_items = definition("test:bad_items", &[]);
        bad_items.supported_items = "#minecraft:no_such_items".to_string();
        assert!(register(bad_items).is_err());
        assert!(Enchantment::from_name("test:bad_items").is_none());
    }

    #[test]
    fn single_item_supported_items() {
        let mut def = definition("test:one_item", &[]);
        def.supported_items = "minecraft:shears".to_string();
        let enchantment = register(def).unwrap();
        assert!(enchantment.can_enchant(&Item::SHEARS));
        assert!(!enchantment.can_enchant(&Item::DIAMOND_PICKAXE));
    }

    #[test]
    fn placeholder_is_claimed_by_the_real_definition() {
        let placeholder = placeholder("test:late").unwrap();
        assert!(Enchantment::from_name("test:late") == Some(placeholder));

        let real = register(definition("test:late", &["minecraft:tradeable"])).unwrap();
        assert_eq!(real.id, placeholder.id);
        assert_eq!(real.max_level, 2);
        assert!(!std::ptr::eq(real, placeholder));
        assert!(std::ptr::eq(
            Enchantment::from_name("test:late").unwrap(),
            real
        ));
        assert!(real.has_tag(&EnchantmentTag::MINECRAFT_TRADEABLE));

        assert!(super::placeholder("minecraft:vanilla_namespace").is_none());
    }

    #[test]
    fn saved_items_keep_unregistered_enchantments() {
        let mut levels = NbtCompound::new();
        levels.put_int("test:saved_before_plugin", 2);
        levels.put_int("minecraft:efficiency", 3);
        let mut components = NbtCompound::new();
        components.put("minecraft:enchantments", NbtTag::Compound(levels));
        let mut compound = NbtCompound::new();
        compound.put_string("id", "minecraft:diamond_pickaxe".to_string());
        compound.put_int("count", 1);
        compound.put("components", NbtTag::Compound(components));

        let stack = ItemStack::read_item_stack(&compound).expect("item must not be dropped");
        let enchantments = stack.get_data_component::<EnchantmentsImpl>().unwrap();
        let saved = Enchantment::from_name("test:saved_before_plugin").unwrap();
        assert!(enchantments.enchantment.contains(&(saved, 2)));
        assert!(
            enchantments
                .enchantment
                .contains(&(&Enchantment::EFFICIENCY, 3))
        );
    }
}
