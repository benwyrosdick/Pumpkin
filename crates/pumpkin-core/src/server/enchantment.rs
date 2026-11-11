use pumpkin_data::Enchantment;
use pumpkin_data::enchantment::{AttributeModifierSlot, Cost};
use pumpkin_data::enchantment_registry::{self, DynamicEnchantment, EnchantmentDefinition};
use pumpkin_data::registry::RegistryEntryData;
use pumpkin_util::text::TextComponent;
use rustc_hash::FxHashMap;
use serde_json::json;
use tokio::sync::RwLock;

use crate::data::datapack::dynamic_registry_loader::json_to_registry_nbt_bytes;

#[derive(Clone, Debug)]
pub struct CustomEnchantmentEntry {
    pub id: String,
    pub description: TextComponent,
    pub max_level: u32,
    pub anvil_cost: u32,
    pub supported_items: String,
    pub weight: u32,
    pub slots: Vec<AttributeModifierSlot>,
    pub exclusive_set: Vec<String>,
    pub options: EnchantmentOptions,
}

/// How a custom enchantment is obtained, beyond being applied directly.
#[derive(Clone, Copy, Debug)]
pub struct EnchantmentOptions {
    /// Enchanting level range for each enchantment level, as in vanilla definitions.
    pub min_cost: Cost,
    pub max_cost: Cost,
    /// Offered by the enchanting table.
    pub in_enchanting_table: bool,
    /// Sold on enchanted books by librarians.
    pub tradeable: bool,
    /// Treasure: never from the enchanting table, and costs double from librarians.
    pub treasure: bool,
    /// A curse: survives the grindstone and shows in red.
    pub curse: bool,
}

impl Default for EnchantmentOptions {
    /// Costs like a common vanilla enchantment (Efficiency), obtainable only by commands
    /// and plugins.
    fn default() -> Self {
        Self {
            min_cost: Cost {
                base: 1,
                per_level_above_first: 10,
            },
            max_cost: Cost {
                base: 51,
                per_level_above_first: 10,
            },
            in_enchanting_table: false,
            tradeable: false,
            treasure: false,
            curse: false,
        }
    }
}

impl CustomEnchantmentEntry {
    fn to_definition(&self) -> Result<EnchantmentDefinition, String> {
        if !(1..=255).contains(&self.max_level) {
            return Err(format!(
                "Enchantment '{}': max level must be between 1 and 255",
                self.id
            ));
        }
        if !(1..=1024).contains(&self.weight) {
            return Err(format!(
                "Enchantment '{}': weight must be between 1 and 1024",
                self.id
            ));
        }

        let options = self.options;
        let mut tags = Vec::new();
        if options.in_enchanting_table {
            tags.extend(["minecraft:in_enchanting_table", "minecraft:non_treasure"]);
        }
        if options.tradeable {
            tags.push("minecraft:tradeable");
        }
        if options.treasure {
            tags.extend(["minecraft:treasure", "minecraft:double_trade_price"]);
        }
        if options.curse {
            tags.push("minecraft:curse");
        }

        Ok(EnchantmentDefinition {
            name: self.id.clone(),
            display_name: self.description.clone().get_text(),
            supported_items: self.supported_items.clone(),
            max_level: self.max_level as i32,
            weight: self.weight as i32,
            anvil_cost: self.anvil_cost,
            slots: self.slots.clone(),
            min_cost: options.min_cost,
            max_cost: options.max_cost,
            exclusive_set: self.exclusive_set.clone(),
            tags: tags.into_iter().map(String::from).collect(),
        })
    }
}

/// Plugin-defined enchantments.
///
/// Each one is also a real runtime enchantment (see [`enchantment_registry`]), so it shows
/// in tooltips and works with the enchanting table, anvil, grindstone, librarians and
/// `/enchant` like a vanilla one.
pub struct EnchantmentManager {
    custom_enchantments: RwLock<FxHashMap<String, CustomEnchantmentEntry>>,
}

impl Default for EnchantmentManager {
    fn default() -> Self {
        Self::new()
    }
}

impl EnchantmentManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            custom_enchantments: RwLock::new(FxHashMap::default()),
        }
    }

    /// Registers a custom enchantment. Registering the same id again (e.g. on a plugin
    /// reload) succeeds and keeps the original definition until the server restarts.
    pub async fn register(&self, enchantment: CustomEnchantmentEntry) -> Result<(), String> {
        enchantment_registry::register(enchantment.to_definition()?)?;
        self.custom_enchantments
            .write()
            .await
            .insert(enchantment.id.clone(), enchantment);
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Option<CustomEnchantmentEntry> {
        let map = self.custom_enchantments.read().await;
        map.get(id).cloned()
    }

    pub async fn has(&self, id: &str) -> bool {
        let map = self.custom_enchantments.read().await;
        map.contains_key(id)
    }

    pub async fn get_all_ids(&self) -> Vec<String> {
        let map = self.custom_enchantments.read().await;
        map.keys().cloned().collect()
    }

    /// The `minecraft:enchantment` registry to send a client, given the vanilla entries
    /// merged with datapacks.
    ///
    /// Item components refer to enchantments by registry index, so runtime enchantments go
    /// straight after vanilla in id order. Datapack-only enchantments, which the server
    /// itself doesn't know, follow. Sending the registry freezes it: clients can't be told
    /// about enchantments added later.
    pub async fn client_registry(
        &self,
        mut merged: Vec<RegistryEntryData>,
    ) -> Vec<RegistryEntryData> {
        enchantment_registry::freeze();
        let dynamic = enchantment_registry::dynamic();
        let mut datapack_only = merged.split_off(Enchantment::ALL.len().min(merged.len()));
        datapack_only.retain(|entry| {
            !dynamic
                .iter()
                .any(|d| d.enchantment.name == entry.entry_id.as_str())
        });

        let definitions = self.custom_enchantments.read().await;
        for enchantment in &dynamic {
            let description = definitions
                .get(enchantment.enchantment.name)
                .map(|entry| &entry.description);
            merged.push(registry_entry(enchantment, description));
        }
        merged.extend(datapack_only);
        merged
    }
}

/// The client registry entry for a runtime enchantment, in the vanilla JSON shape.
fn registry_entry(
    dynamic: &DynamicEnchantment,
    description: Option<&TextComponent>,
) -> RegistryEntryData {
    let enchantment = dynamic.enchantment;
    let description = description.map_or_else(
        || json!({ "text": enchantment.description }),
        |text| {
            serde_json::to_value(text)
                .unwrap_or_else(|_| json!({ "text": enchantment.description }))
        },
    );
    let cost = |cost: &Cost| json!({ "base": cost.base, "per_level_above_first": cost.per_level_above_first });
    let definition = json!({
        "description": description,
        "supported_items": dynamic.supported_items,
        "weight": enchantment.weight,
        "max_level": enchantment.max_level,
        "min_cost": cost(&enchantment.min_cost),
        "max_cost": cost(&enchantment.max_cost),
        "anvil_cost": enchantment.anvil_cost,
        "slots": enchantment.slots.iter().map(slot_name).collect::<Vec<_>>(),
    });
    RegistryEntryData {
        entry_id: enchantment.name.to_string(),
        data: Some(json_to_registry_nbt_bytes(&definition)),
    }
}

const fn slot_name(slot: &AttributeModifierSlot) -> &'static str {
    match slot {
        AttributeModifierSlot::Any => "any",
        AttributeModifierSlot::MainHand => "mainhand",
        AttributeModifierSlot::OffHand => "offhand",
        AttributeModifierSlot::Hand => "hand",
        AttributeModifierSlot::Feet => "feet",
        AttributeModifierSlot::Legs => "legs",
        AttributeModifierSlot::Chest => "chest",
        AttributeModifierSlot::Head => "head",
        AttributeModifierSlot::Armor => "armor",
        AttributeModifierSlot::Body => "body",
        AttributeModifierSlot::Saddle => "saddle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_data::packet::CURRENT_MC_VERSION;
    use pumpkin_data::registry::Registry;
    use pumpkin_nbt::Nbt;
    use pumpkin_nbt::compound::NbtCompound;

    fn decode(entry: &RegistryEntryData) -> NbtCompound {
        let bytes = entry
            .data
            .as_ref()
            .expect("runtime entries carry full data");
        let mut cursor = std::io::Cursor::new(&bytes[..]);
        let mut reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(
            pumpkin_nbt::deserializer::NbtStreamReader(&mut cursor),
        );
        Nbt::read_unnamed(&mut reader).unwrap().root_tag
    }

    #[tokio::test]
    async fn client_registry_puts_runtime_enchantments_at_their_ids() {
        let manager = EnchantmentManager::new();
        manager
            .register(CustomEnchantmentEntry {
                id: "test_core:area".to_string(),
                description: TextComponent::text("Area"),
                max_level: 2,
                anvil_cost: 4,
                supported_items: "#minecraft:pickaxes".to_string(),
                weight: 2,
                slots: vec![AttributeModifierSlot::MainHand],
                exclusive_set: Vec::new(),
                options: EnchantmentOptions {
                    in_enchanting_table: true,
                    tradeable: true,
                    ..EnchantmentOptions::default()
                },
            })
            .await
            .unwrap();
        // A second registration (plugin reload) is fine.
        manager
            .register(manager.get("test_core:area").await.unwrap())
            .await
            .unwrap();

        let vanilla = Registry::get_synced(CURRENT_MC_VERSION)
            .into_iter()
            .find(|r| r.registry_id == "minecraft:enchantment")
            .unwrap()
            .registry_entries;
        let sent = manager.client_registry(vanilla).await;

        // Every server-side enchantment id is its index in what the client receives.
        for enchantment in enchantment_registry::all() {
            let entry = &sent[usize::from(enchantment.id)];
            let expected = if usize::from(enchantment.id) < Enchantment::ALL.len() {
                format!("minecraft:{}", enchantment.registry_key)
            } else {
                enchantment.name.to_string()
            };
            assert_eq!(entry.entry_id, expected);
        }

        let area = Enchantment::from_name("test_core:area").unwrap();
        let nbt = decode(&sent[usize::from(area.id)]);
        assert_eq!(
            nbt.get_string("supported_items"),
            Some("#minecraft:pickaxes")
        );
        assert_eq!(nbt.get_int("max_level"), Some(2));
        assert_eq!(nbt.get_int("weight"), Some(2));
        assert_eq!(nbt.get_int("anvil_cost"), Some(4));
        assert_eq!(
            nbt.get_compound("min_cost").and_then(|c| c.get_int("base")),
            Some(1)
        );
        assert!(nbt.get_compound("description").is_some());
        assert_eq!(nbt.get_list("slots").map(<[_]>::len), Some(1));

        // Sending froze the registry.
        assert!(enchantment_registry::is_frozen());
    }
}
