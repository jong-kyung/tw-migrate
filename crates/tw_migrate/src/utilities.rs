use std::collections::HashMap;

use crate::{arbitrary::encode as arbitrary_value, theme::exact_theme_token};

#[derive(Default)]
pub(crate) struct SpacingValues {
    values: [Option<String>; 4],
    used: bool,
}

impl SpacingValues {
    pub(crate) fn apply(
        &mut self,
        property: &str,
        family: &str,
        value: &str,
        components: &[&str],
    ) -> Result<bool, ()> {
        if property == family {
            let sides = match components {
                [all] => [*all, *all, *all, *all],
                [vertical, horizontal] => [*vertical, *horizontal, *vertical, *horizontal],
                [top, horizontal, bottom] => [*top, *horizontal, *bottom, *horizontal],
                [top, right, bottom, left] => [*top, *right, *bottom, *left],
                _ => return Err(()),
            };
            for (target, value) in self.values.iter_mut().zip(sides) {
                *target = Some(value.to_string());
            }
            self.used = true;
            return Ok(true);
        }

        let side = match property.strip_prefix(&format!("{family}-")) {
            Some("top") => 0,
            Some("right") => 1,
            Some("bottom") => 2,
            Some("left") => 3,
            _ => return Ok(false),
        };
        self.values[side] = Some(value.to_string());
        self.used = true;
        Ok(true)
    }

    pub(crate) fn candidates(
        &self,
        family_prefix: &str,
        theme_tokens: &HashMap<String, String>,
    ) -> Vec<String> {
        if !self.used {
            return Vec::new();
        }
        if let [Some(top), Some(right), Some(bottom), Some(left)] = &self.values
            && top == right
            && top == bottom
            && top == left
        {
            return vec![themed_candidate(
                family_prefix,
                "spacing",
                top,
                theme_tokens,
            )];
        }
        ["t", "r", "b", "l"]
            .into_iter()
            .zip(&self.values)
            .filter_map(|(side, value)| {
                value.as_ref().map(|value| {
                    themed_candidate(
                        &format!("{family_prefix}{side}"),
                        "spacing",
                        value,
                        theme_tokens,
                    )
                })
            })
            .collect()
    }
}

pub(crate) fn declaration_to_candidate(
    property: &str,
    value: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    if value.is_empty() || value.contains(['[', ']', ';']) {
        return None;
    }
    let static_candidate = match (property, value) {
        ("display", "flex") => Some("flex"),
        ("display", "grid") => Some("grid"),
        ("display", "none") => Some("hidden"),
        _ => None,
    };
    if let Some(candidate) = static_candidate {
        return Some(candidate.to_string());
    }

    let (prefix, token_namespace) = match property {
        "padding" => ("p", Some("spacing")),
        "margin" => ("m", Some("spacing")),
        "gap" => ("gap", Some("spacing")),
        "width" => ("w", Some("spacing")),
        "height" => ("h", Some("spacing")),
        "color" => ("text", Some("color")),
        "background-color" => ("bg", Some("color")),
        "border-radius" => ("rounded", Some("radius")),
        "font-size" => ("text", Some("text")),
        _ => return Some(format!("[{property}:{}]", arbitrary_value(value))),
    };
    Some(themed_candidate(
        prefix,
        token_namespace.expect("mapped utility namespace"),
        value,
        theme_tokens,
    ))
}

pub(crate) fn tailwind_utilities_conflict(generated: &str, existing: &str) -> bool {
    if generated == existing {
        return false;
    }
    let (generated_variants, generated_utility) = tailwind_utility_parts(generated);
    let (existing_variants, existing_utility) = tailwind_utility_parts(existing);
    if generated_variants != existing_variants {
        return false;
    }

    let generated_properties = utility_property_mask(generated_utility)
        | arbitrary_utility_property(generated_utility).map_or(0, arbitrary_property_mask);
    let existing_properties = utility_property_mask(existing_utility)
        | arbitrary_utility_property(existing_utility).map_or(0, arbitrary_property_mask);
    if generated_properties & existing_properties != 0 {
        return true;
    }
    matches!(
        (
            arbitrary_utility_property(generated_utility),
            arbitrary_utility_property(existing_utility)
        ),
        (Some(generated), Some(existing)) if generated == existing
    )
}

pub(crate) fn tailwind_variants_match(left: &str, right: &str) -> bool {
    tailwind_utility_parts(left).0 == tailwind_utility_parts(right).0
}

pub(crate) fn css_properties_conflict(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    if left == "all" {
        return reset_by_all(right);
    }
    if right == "all" {
        return reset_by_all(left);
    }
    let left_mask = arbitrary_property_mask(left);
    let right_mask = arbitrary_property_mask(right);
    if left_mask != 0 && left_mask & right_mask != 0 {
        return true;
    }
    let left_border = border_property_mask(left);
    let right_border = border_property_mask(right);
    if left_border != 0 && left_border & right_border != 0 {
        return true;
    }
    shorthand_contains(left, right) || shorthand_contains(right, left)
}

fn reset_by_all(property: &str) -> bool {
    !property.starts_with("--") && !matches!(property, "direction" | "unicode-bidi")
}

fn shorthand_contains(shorthand: &str, longhand: &str) -> bool {
    let longhands: &[&str] = match shorthand {
        "animation" => &[
            "animation-delay",
            "animation-direction",
            "animation-duration",
            "animation-fill-mode",
            "animation-iteration-count",
            "animation-name",
            "animation-play-state",
            "animation-timing-function",
        ],
        "background" => &[
            "background-attachment",
            "background-clip",
            "background-color",
            "background-image",
            "background-origin",
            "background-position",
            "background-repeat",
            "background-size",
        ],
        "column-rule" => &[
            "column-rule-color",
            "column-rule-style",
            "column-rule-width",
        ],
        "columns" => &["column-count", "column-width"],
        "flex" => &["flex-basis", "flex-grow", "flex-shrink"],
        "flex-flow" => &["flex-direction", "flex-wrap"],
        "font" => &[
            "font-family",
            "font-size",
            "font-stretch",
            "font-style",
            "font-variant",
            "font-weight",
            "line-height",
        ],
        "grid" => &[
            "grid-template-rows",
            "grid-template-columns",
            "grid-template-areas",
            "grid-auto-rows",
            "grid-auto-columns",
            "grid-auto-flow",
            "row-gap",
            "column-gap",
        ],
        "grid-area" => &[
            "grid-row-start",
            "grid-column-start",
            "grid-row-end",
            "grid-column-end",
        ],
        "grid-column" => &["grid-column-start", "grid-column-end"],
        "grid-row" => &["grid-row-start", "grid-row-end"],
        "grid-template" => &[
            "grid-template-rows",
            "grid-template-columns",
            "grid-template-areas",
        ],
        "inset" => &["top", "right", "bottom", "left"],
        "inset-block" => &["inset-block-start", "inset-block-end"],
        "inset-inline" => &["inset-inline-start", "inset-inline-end"],
        "list-style" => &["list-style-image", "list-style-position", "list-style-type"],
        "outline" => &["outline-color", "outline-style", "outline-width"],
        "overflow" => &["overflow-x", "overflow-y"],
        "place-content" => &["align-content", "justify-content"],
        "place-items" => &["align-items", "justify-items"],
        "place-self" => &["align-self", "justify-self"],
        "text-decoration" => &[
            "text-decoration-color",
            "text-decoration-line",
            "text-decoration-style",
            "text-decoration-thickness",
        ],
        "transition" => &[
            "transition-behavior",
            "transition-delay",
            "transition-duration",
            "transition-property",
            "transition-timing-function",
        ],
        _ => &[],
    };
    longhands.contains(&longhand)
}

fn border_property_mask(property: &str) -> u16 {
    const WIDTH: u16 = 0b0000_0000_1111;
    const STYLE: u16 = 0b0000_1111_0000;
    const COLOR: u16 = 0b1111_0000_0000;
    const IMAGE: u16 = 1 << 12;
    const TOP: u16 = 0b0001_0001_0001;
    const RIGHT: u16 = 0b0010_0010_0010;
    const BOTTOM: u16 = 0b0100_0100_0100;
    const LEFT: u16 = 0b1000_1000_1000;
    const BLOCK: u16 = TOP | BOTTOM;
    const INLINE: u16 = RIGHT | LEFT;
    const ALL: u16 = WIDTH | STYLE | COLOR | IMAGE;

    match property {
        "border" => ALL,
        "border-width" => WIDTH,
        "border-style" => STYLE,
        "border-color" => COLOR,
        "border-top" => TOP,
        "border-right" => RIGHT,
        "border-bottom" => BOTTOM,
        "border-left" => LEFT,
        "border-block" => BLOCK,
        "border-inline" => INLINE,
        "border-block-start" | "border-block-end" => BLOCK,
        "border-inline-start" | "border-inline-end" => INLINE,
        "border-top-width" => TOP & WIDTH,
        "border-right-width" => RIGHT & WIDTH,
        "border-bottom-width" => BOTTOM & WIDTH,
        "border-left-width" => LEFT & WIDTH,
        "border-block-width" | "border-block-start-width" | "border-block-end-width" => {
            BLOCK & WIDTH
        }
        "border-inline-width" | "border-inline-start-width" | "border-inline-end-width" => {
            INLINE & WIDTH
        }
        "border-top-style" => TOP & STYLE,
        "border-right-style" => RIGHT & STYLE,
        "border-bottom-style" => BOTTOM & STYLE,
        "border-left-style" => LEFT & STYLE,
        "border-block-style" | "border-block-start-style" | "border-block-end-style" => {
            BLOCK & STYLE
        }
        "border-inline-style" | "border-inline-start-style" | "border-inline-end-style" => {
            INLINE & STYLE
        }
        "border-top-color" => TOP & COLOR,
        "border-right-color" => RIGHT & COLOR,
        "border-bottom-color" => BOTTOM & COLOR,
        "border-left-color" => LEFT & COLOR,
        "border-block-color" | "border-block-start-color" | "border-block-end-color" => {
            BLOCK & COLOR
        }
        "border-inline-color" | "border-inline-start-color" | "border-inline-end-color" => {
            INLINE & COLOR
        }
        "border-image"
        | "border-image-source"
        | "border-image-slice"
        | "border-image-width"
        | "border-image-outset"
        | "border-image-repeat" => IMAGE,
        _ => 0,
    }
}

fn themed_candidate(
    prefix: &str,
    namespace: &str,
    value: &str,
    theme_tokens: &HashMap<String, String>,
) -> String {
    if let Some(name) = exact_theme_token(namespace, value, theme_tokens) {
        format!("{prefix}-{name}")
    } else {
        format!("{prefix}-[{}]", arbitrary_value(value))
    }
}

fn tailwind_utility_parts(class: &str) -> (&str, &str) {
    let mut depth = 0usize;
    let mut separator = None;
    for (index, character) in class.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => separator = Some(index),
            _ => {}
        }
    }
    separator.map_or(("", class), |index| (&class[..index], &class[index + 1..]))
}

fn utility_property_mask(utility: &str) -> u32 {
    let utility = utility.trim_matches('!');
    let utility = utility.strip_prefix('-').unwrap_or(utility);
    if let Some(mask) = spacing_utility_mask(utility, 'p', 0) {
        return mask;
    }
    if let Some(mask) = spacing_utility_mask(utility, 'm', 6) {
        return mask;
    }
    match utility {
        utility if utility.starts_with("gap-x-") => 1 << 13,
        utility if utility.starts_with("gap-y-") => 1 << 12,
        utility if utility.starts_with("gap-") => 3 << 12,
        utility if utility.starts_with("w-") => 1 << 14,
        utility if utility.starts_with("h-") => 1 << 15,
        utility if utility.starts_with("size-") => 3 << 14,
        utility if utility == "rounded" || utility.starts_with("rounded-") => {
            rounded_utility_mask(utility) << 16
        }
        "block" | "inline" | "inline-block" | "flow-root" | "flex" | "inline-flex" | "grid"
        | "inline-grid" | "contents" | "table" | "hidden" => 1 << 20,
        _ => 0,
    }
}

fn arbitrary_property_mask(property: &str) -> u32 {
    // Maps an arbitrary-property candidate ([display:block]) onto the same
    // bit masks as named utilities so mixed pairs like `[display:block]` vs
    // `hidden` register as conflicts. Logical inline/block sides use the
    // broader physical masks on purpose -- over-matching only retains rules.
    match property {
        "padding" => 0b111111,
        "padding-top" => 0b000001,
        "padding-right" => 0b000010,
        "padding-bottom" => 0b000100,
        "padding-left" => 0b001000,
        "padding-inline" | "padding-inline-start" | "padding-inline-end" => 0b111010,
        "padding-block" | "padding-block-start" | "padding-block-end" => 0b000101,
        "margin" => 0b111111 << 6,
        "margin-top" => 0b000001 << 6,
        "margin-right" => 0b000010 << 6,
        "margin-bottom" => 0b000100 << 6,
        "margin-left" => 0b001000 << 6,
        "margin-inline" | "margin-inline-start" | "margin-inline-end" => 0b111010 << 6,
        "margin-block" | "margin-block-start" | "margin-block-end" => 0b000101 << 6,
        "row-gap" => 1 << 12,
        "column-gap" => 1 << 13,
        "gap" => 3 << 12,
        "width" => 1 << 14,
        "height" => 1 << 15,
        "border-radius" => 0b1111 << 16,
        "border-top-left-radius" | "border-start-start-radius" => 0b0001 << 16,
        "border-top-right-radius" | "border-start-end-radius" => 0b0010 << 16,
        "border-bottom-right-radius" | "border-end-end-radius" => 0b0100 << 16,
        "border-bottom-left-radius" | "border-end-start-radius" => 0b1000 << 16,
        "display" => 1 << 20,
        _ => 0,
    }
}

fn spacing_utility_mask(utility: &str, prefix: char, shift: u32) -> Option<u32> {
    let (side, _) = utility.strip_prefix(prefix)?.split_once('-')?;
    let mask = match side {
        "" => 0b111111,
        "x" => 0b111010,
        "y" => 0b000101,
        "t" => 0b000001,
        "r" => 0b000010,
        "b" => 0b000100,
        "l" => 0b001000,
        "s" => 0b010000,
        "e" => 0b100000,
        _ => return None,
    };
    Some(mask << shift)
}

fn rounded_utility_mask(utility: &str) -> u32 {
    let side = utility
        .strip_prefix("rounded-")
        .and_then(|utility| utility.split('-').next());
    match side {
        Some("t") => 0b0011,
        Some("r") => 0b0110,
        Some("b") => 0b1100,
        Some("l") => 0b1001,
        Some("tl" | "ss") => 0b0001,
        Some("tr" | "se") => 0b0010,
        Some("br" | "ee") => 0b0100,
        Some("bl" | "es") => 0b1000,
        Some("s") => 0b1001,
        Some("e") => 0b0110,
        _ => 0b1111,
    }
}

fn arbitrary_utility_property(utility: &str) -> Option<&str> {
    utility
        .trim_matches('!')
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split_once(':')
        .map(|(property, _)| property.trim())
}
