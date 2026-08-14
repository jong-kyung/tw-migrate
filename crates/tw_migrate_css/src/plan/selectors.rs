use std::collections::HashSet;

use oxc_css_parser::{
    Syntax,
    ast::{
        CombinatorKind, ComplexSelectorChild, CompoundSelector, InterpolableIdent, SimpleSelector,
        Statement, TypeSelector,
    },
};

use crate::{arbitrary::encode as encode_arbitrary, at_rules::parse_css};

use super::{ModuleRelationship, Relation, RelationshipStep, SelectorKey};

/// Selector surface of the package's non-scoped CSS: the classes, ids, and
/// element types its rules can match, plus whether anything defied
/// classification. Used to decide if deleting a Vue scoped rule could hand
/// the cascade to an unlayered competitor.
#[derive(Default)]
pub struct ShadowIndex {
    pub classes: HashSet<String>,
    pub ids: HashSet<String>,
    pub types: HashSet<String>,
    pub unverifiable: bool,
}

pub fn index_shadow_selectors(pieces: &[String], module_pieces: &[String]) -> ShadowIndex {
    let mut index = ShadowIndex::default();
    for (piece, module) in pieces
        .iter()
        .map(|piece| (piece, false))
        .chain(module_pieces.iter().map(|piece| (piece, true)))
    {
        let allocator = oxc_css_parser::Allocator::default();
        match parse_css(&allocator, piece, Syntax::Css) {
            Ok(stylesheet) => index_shadow_statements(&stylesheet.statements, module, &mut index),
            // A piece that is not plain CSS cannot prove its selectors.
            Err(_) => index.unverifiable = true,
        }
    }
    index
}

fn index_shadow_statements(statements: &[Statement<'_>], module: bool, index: &mut ShadowIndex) {
    for statement in statements {
        match statement {
            Statement::QualifiedRule(rule) => {
                for selector in &rule.selector.selectors {
                    index_shadow_complex(selector, module, index);
                }
                index_shadow_statements(&rule.block.statements, module, index);
            }
            Statement::AtRule(at_rule) => {
                if let Some(block) = &at_rule.block {
                    index_shadow_statements(&block.statements, module, index);
                }
            }
            // Declarations and keyframe steps cannot match DOM elements.
            Statement::Declaration(_) | Statement::KeyframeBlock(_) => {}
            _ => index.unverifiable = true,
        }
    }
}

/// Classify one complex selector by its rightmost compound: the compound
/// that must match the element itself. Ancestor compounds only narrow the
/// match, so they are ignored (conservatively assuming they can be
/// satisfied).
fn index_shadow_complex(
    selector: &oxc_css_parser::ast::ComplexSelector<'_>,
    module: bool,
    index: &mut ShadowIndex,
) {
    // Selector-mode `:global .card` changes the scope of a later compound.
    // The rightmost-compound index does not carry that state, so retain
    // conservatively rather than misclassifying `.card` as module-local.
    if module
        && selector.children.iter().any(|child| {
            let ComplexSelectorChild::CompoundSelector(compound) = child else {
                return false;
            };
            compound.children.iter().any(|simple| {
                matches!(simple, SimpleSelector::PseudoClass(pseudo)
                    if literal_ident(&pseudo.name) == Some("global") && pseudo.arg.is_none())
            })
        })
    {
        index.unverifiable = true;
        return;
    }
    let Some(compound) = selector
        .children
        .iter()
        .rev()
        .find_map(|child| match child {
            ComplexSelectorChild::CompoundSelector(compound) => Some(compound),
            ComplexSelectorChild::Combinator(_) => None,
        })
    else {
        index.unverifiable = true;
        return;
    };
    index_shadow_compound(compound, module, index);
}

fn index_shadow_compound(compound: &CompoundSelector<'_>, module: bool, index: &mut ShadowIndex) {
    // CSS Modules localize class and id names, so a module compound carrying
    // either can never match a template element; only its bare type and
    // attribute selectors stay global.
    if module
        && compound
            .children
            .iter()
            .any(|simple| matches!(simple, SimpleSelector::Class(_) | SimpleSelector::Id(_)))
    {
        return;
    }
    let mut classes = Vec::new();
    let mut ids = Vec::new();
    let mut types = Vec::new();
    let mut base_free_but_bounded = false;
    for simple in &compound.children {
        match simple {
            SimpleSelector::Class(class) => match literal_ident(&class.name) {
                Some(name) => classes.push(name.to_string()),
                None => index.unverifiable = true,
            },
            SimpleSelector::Id(id) => match literal_ident(&id.name) {
                Some(name) => ids.push(name.to_string()),
                None => index.unverifiable = true,
            },
            SimpleSelector::Type(TypeSelector::TagName(tag)) => {
                match literal_ident(&tag.name.name) {
                    Some(name) => types.push(name.to_ascii_lowercase()),
                    None => index.unverifiable = true,
                }
            }
            SimpleSelector::Type(TypeSelector::Universal(_)) => index.unverifiable = true,
            // Attribute selectors and pseudo-classes only narrow a match
            // that already has a base; pseudo-elements style separate boxes.
            SimpleSelector::Attribute(_) | SimpleSelector::PseudoElement(_) => {}
            SimpleSelector::PseudoClass(pseudo) => match literal_ident(&pseudo.name) {
                // `:root` alone can only match the document element, which a
                // template can never contain.
                Some("root") if pseudo.arg.is_none() => base_free_but_bounded = true,
                // `:global(...)` re-exposes its argument as plain global
                // selectors; `:local(...)` content stays localized.
                Some("global") if pseudo.arg.is_some() => {
                    index_shadow_global_arg(pseudo.arg.as_ref().expect("checked"), index);
                    base_free_but_bounded = true;
                }
                Some("local") if pseudo.arg.is_some() => base_free_but_bounded = true,
                _ => {}
            },
            SimpleSelector::Nesting(_) | SimpleSelector::SassPlaceholder(_) => {
                index.unverifiable = true;
            }
        }
    }
    if !classes.is_empty() {
        index.classes.extend(classes);
    } else if !ids.is_empty() {
        index.ids.extend(ids);
    } else if !types.is_empty() {
        index.types.extend(types);
    } else if !base_free_but_bounded {
        // A compound with no class/id/type base (bare pseudo-class or
        // attribute selector) can match arbitrary elements.
        index.unverifiable = true;
    }
}

fn index_shadow_global_arg(
    arg: &oxc_css_parser::ast::PseudoClassSelectorArg<'_>,
    index: &mut ShadowIndex,
) {
    use oxc_css_parser::ast::PseudoClassSelectorArgKind;
    match &arg.kind {
        PseudoClassSelectorArgKind::CompoundSelectorList(list) => {
            for compound in &list.selectors {
                index_shadow_compound(compound, false, index);
            }
        }
        PseudoClassSelectorArgKind::SelectorList(list) => {
            for selector in &list.selectors {
                index_shadow_complex(selector, false, index);
            }
        }
        _ => index.unverifiable = true,
    }
}

pub(super) fn declaration_value<'a>(
    source: &'a str,
    declaration: &oxc_css_parser::ast::Declaration<'_>,
) -> &'a str {
    source[declaration.colon_span.end..declaration.span.end]
        .trim()
        .trim_end_matches(';')
        .trim()
}

pub(super) fn selector_match(
    rule: &oxc_css_parser::ast::QualifiedRule<'_>,
    source: &str,
    is_module: bool,
) -> Option<(SelectorKey, Option<String>)> {
    let selector = rule.selector.selectors.first()?;
    if rule.selector.selectors.len() != 1 {
        return None;
    }

    if selector.children.len() == 1 {
        let ComplexSelectorChild::CompoundSelector(compound) = &selector.children[0] else {
            return None;
        };
        if let Some(result) = compound_key_variant(compound) {
            return Some(result);
        }
        // A key plus one argument-less pseudo-class was already rejected above
        // (unsupported state); it must not fall through to an arbitrary variant.
        if is_module
            || matches!(
                compound.children.as_slice(),
                [_, SimpleSelector::PseudoClass(pseudo)] if pseudo.arg.is_none()
            )
        {
            return None;
        }
        let key = selector_key(compound.children.first()?)?;
        let variant = arbitrary_selector_variant(rule, source, compound)?;
        return Some((key, Some(variant)));
    }

    if is_module {
        return None;
    }
    let ComplexSelectorChild::CompoundSelector(target) = selector.children.last()? else {
        return None;
    };
    let key = selector_key(target.children.first()?)?;
    let variant = arbitrary_selector_variant(rule, source, target)?;
    Some((key, Some(variant)))
}

fn supported_pseudo_state(name: &str) -> bool {
    matches!(
        name,
        "active" | "disabled" | "focus" | "focus-visible" | "focus-within" | "hover" | "visited"
    )
}

/// A compound that is a single module class/id key, optionally followed by
/// one supported pseudo-state.
fn compound_key_variant(compound: &CompoundSelector<'_>) -> Option<(SelectorKey, Option<String>)> {
    let key = selector_key(compound.children.first()?)?;
    let variant = match compound.children.as_slice() {
        [_] => None,
        [_, SimpleSelector::PseudoClass(pseudo)] if pseudo.arg.is_none() => {
            let name = literal_ident(&pseudo.name)?;
            if !supported_pseudo_state(name) {
                return None;
            }
            Some(name.to_string())
        }
        _ => return None,
    };
    Some((key, variant))
}

/// Decompose a multi-compound CSS Module selector (descendant/child chains of
/// module keys) into its target key, target variant, and proof obligations.
/// Anything outside that shape returns None and stays on the generic
/// unsupported-selector path.
pub(super) fn module_relationship_match(
    rule: &oxc_css_parser::ast::QualifiedRule<'_>,
) -> Option<(SelectorKey, Option<String>, ModuleRelationship)> {
    if rule.selector.selectors.len() != 1 {
        return None;
    }
    let selector = rule.selector.selectors.first()?;
    let mut parts: Vec<(SelectorKey, Option<String>)> = Vec::new();
    let mut relations: Vec<Relation> = Vec::new();
    let mut expect_compound = true;
    for child in &selector.children {
        match child {
            ComplexSelectorChild::CompoundSelector(compound) => {
                if !expect_compound {
                    return None;
                }
                parts.push(compound_key_variant(compound)?);
                expect_compound = false;
            }
            ComplexSelectorChild::Combinator(combinator) => {
                if expect_compound {
                    return None;
                }
                relations.push(match combinator.kind {
                    CombinatorKind::Descendant => Relation::Descendant,
                    CombinatorKind::Child => Relation::Child,
                    _ => return None,
                });
                expect_compound = true;
            }
        }
    }
    if expect_compound || parts.len() < 2 || parts.len() != relations.len() + 1 {
        return None;
    }
    let ancestor_state = parts[..parts.len() - 1]
        .iter()
        .any(|(_, variant)| variant.is_some());
    let steps = (1..parts.len())
        .rev()
        .map(|index| RelationshipStep {
            ancestor: parts[index - 1].0.clone(),
            relation: relations[index - 1],
            target: parts[index].0.clone(),
        })
        .collect();
    let (target_key, target_variant) = parts.pop()?;
    Some((
        target_key,
        target_variant,
        ModuleRelationship {
            steps,
            ancestor_state,
        },
    ))
}

fn selector_key(selector: &SimpleSelector<'_>) -> Option<SelectorKey> {
    match selector {
        SimpleSelector::Class(class) => {
            literal_ident(&class.name).map(|name| SelectorKey::Class(name.to_string()))
        }
        SimpleSelector::Id(id) => {
            literal_ident(&id.name).map(|name| SelectorKey::Id(name.to_string()))
        }
        _ => None,
    }
}

fn arbitrary_selector_variant(
    rule: &oxc_css_parser::ast::QualifiedRule<'_>,
    source: &str,
    target: &oxc_css_parser::ast::CompoundSelector<'_>,
) -> Option<String> {
    // Replace the target simple selector by its parsed span. Searching the
    // selector text for ".name" matched the wrong occurrence when the name
    // recurred later (e.g. inside `:not(.abc)` for `.a:not(.abc)`).
    let target_span = match target.children.first()? {
        SimpleSelector::Class(class) if literal_ident(&class.name).is_some() => class.span,
        SimpleSelector::Id(id) if literal_ident(&id.name).is_some() => id.span,
        _ => return None,
    };
    let selector_span = rule.selector.span;
    let mut condition = source[selector_span.start..selector_span.end].to_string();
    condition.replace_range(
        target_span.start - selector_span.start..target_span.end - selector_span.start,
        "&",
    );
    Some(format!("[{}]", encode_arbitrary(&condition)))
}

pub(super) fn literal_ident<'a>(ident: &'a InterpolableIdent<'a>) -> Option<&'a str> {
    match ident {
        InterpolableIdent::Literal(ident) => Some(ident.name),
        _ => None,
    }
}
