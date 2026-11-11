//! The runtime enchantment registry once it has been sent to a client. Freezing is
//! process-wide, so this lives in its own test binary.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use pumpkin_data::data_component_impl::EnchantmentsImpl;
use pumpkin_data::enchantment::{AttributeModifierSlot, Cost};
use pumpkin_data::enchantment_registry::{self, EnchantmentDefinition};
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{Enchantment, item::Item};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;

fn definition(name: &str) -> EnchantmentDefinition {
    EnchantmentDefinition {
        name: name.to_string(),
        display_name: "Test".to_string(),
        supported_items: "#minecraft:axes".to_string(),
        max_level: 1,
        weight: 2,
        anvil_cost: 4,
        slots: vec![AttributeModifierSlot::MainHand],
        min_cost: Cost {
            base: 15,
            per_level_above_first: 0,
        },
        max_cost: Cost {
            base: 65,
            per_level_above_first: 0,
        },
        exclusive_set: Vec::new(),
        tags: Vec::new(),
    }
}

fn saved_axe(levels: NbtCompound) -> NbtCompound {
    let mut components = NbtCompound::new();
    components.put("minecraft:enchantments", NbtTag::Compound(levels));
    let mut compound = NbtCompound::new();
    compound.put_string("id", "minecraft:iron_axe".to_string());
    compound.put_int("count", 1);
    compound.put("components", NbtTag::Compound(components));
    compound
}

#[test]
fn frozen_registry() {
    let before = enchantment_registry::register(definition("test:before_freeze")).unwrap();
    enchantment_registry::freeze();

    // Existing enchantments still register (plugin reloads); new ones need a restart.
    let again = enchantment_registry::register(definition("test:before_freeze")).unwrap();
    assert!(std::ptr::eq(before, again));
    let err = enchantment_registry::register(definition("test:after_freeze"))
        .err()
        .expect("new enchantments must be rejected once frozen");
    assert!(err.contains("restart"), "{err}");
    assert!(enchantment_registry::placeholder("test:after_freeze").is_none());

    // An item with an enchantment nobody registered keeps it in custom data instead of
    // being dropped, and gets it back once it resolves.
    let mut levels = NbtCompound::new();
    levels.put_int("test:never_registered", 1);
    levels.put_int("test:before_freeze", 1);
    let stack = ItemStack::read_item_stack(&saved_axe(levels)).expect("item must not be dropped");
    assert_eq!(stack.item.id, Item::IRON_AXE.id);
    let enchantments = stack.get_data_component::<EnchantmentsImpl>().unwrap();
    assert!(enchantments.enchantment.as_ref() == [(before, 1)]);
    assert_eq!(
        stack.get_custom_data("pumpkin:unknown_enchantments", "test:never_registered"),
        Some(NbtTag::Int(1))
    );

    // Saving and loading again round-trips the kept enchantment.
    let mut saved = NbtCompound::new();
    stack.write_item_stack(&mut saved);
    let reloaded = ItemStack::read_item_stack(&saved).expect("item must not be dropped");
    assert_eq!(
        reloaded.get_custom_data("pumpkin:unknown_enchantments", "test:never_registered"),
        Some(NbtTag::Int(1))
    );
    assert!(Enchantment::from_name("test:never_registered").is_none());
}
