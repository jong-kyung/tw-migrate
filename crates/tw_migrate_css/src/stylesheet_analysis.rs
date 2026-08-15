//! Structured stylesheet facts for the TypeScript orchestration layer:
//! dependency references, loading-import spans, scoped-escape selectors,
//! theme tokens, and global at-rule identities, collected from
//! oxc-css-parser nodes so comments and string contents never count.

use std::collections::BTreeMap;

use serde::Serialize;
use tw_migrate_error::{MigrationError, MigrationResult};

use crate::at_rules::parse_css;
use oxc_css_parser::Syntax;
use oxc_css_parser::ast::{
    AtRulePrelude, AttributeSelectorMatcherKind, AttributeSelectorValue, ColorProfilePrelude,
    CombinatorKind, ComplexSelectorChild, ComponentValue, CompoundSelector, FontFamilyName,
    ImportPrelude, ImportPreludeHref, InterpolableIdent, InterpolableStr,
    PseudoClassSelectorArgKind, SimpleSelector, Statement, UrlValue,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StylesheetImport {
    href: String,
    media: String,
    start: usize,
    end: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StylesheetAnalysis {
    references: Vec<String>,
    imports: Vec<StylesheetImport>,
    unverifiable: bool,
    scope_escapes: Vec<String>,
    scope_shadow_css: Vec<String>,
    scope_escapes_unverifiable: bool,
    selectors_unverifiable: bool,
    theme_tokens: BTreeMap<String, String>,
    global_at_rule_identities: Vec<String>,
    global_at_rules_unverifiable: bool,
    /// Authored class spellings this stylesheet's selectors can match:
    /// literal class selectors plus exact and word-matching class-attribute
    /// selector values. Canonicalization reserves these spellings.
    class_names: Vec<String>,
    /// True when a selector's class match set cannot be bounded: an
    /// interpolated class name, or a class-attribute matcher such as
    /// `[class*=]` whose semantics exceed word equality.
    class_reservations_unbounded: bool,
}

fn stylesheet_syntax(path: &str) -> Result<Syntax, String> {
    match std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("css") => Ok(Syntax::Css),
        Some("scss") => Ok(Syntax::Scss),
        Some("sass") => Ok(Syntax::Sass),
        Some("less") => Ok(Syntax::Less),
        _ => Err(format!("Unsupported stylesheet: {path}")),
    }
}

pub(crate) fn literal_str(value: &InterpolableStr<'_>) -> Option<String> {
    match value {
        InterpolableStr::Literal(literal) => Some(literal.value.to_string()),
        _ => None,
    }
}

pub(crate) fn import_href(href: &ImportPreludeHref<'_>) -> Option<String> {
    match href {
        ImportPreludeHref::Str(value) => literal_str(value),
        ImportPreludeHref::Url(url) => match &url.value {
            Some(UrlValue::Raw(raw)) => Some(raw.value.to_string()),
            Some(UrlValue::Str(value)) => literal_str(value),
            _ => None,
        },
        ImportPreludeHref::Function(_) => None,
    }
}

fn interpolable_str_end(value: &InterpolableStr<'_>) -> usize {
    match value {
        InterpolableStr::Literal(value) => value.span.end,
        InterpolableStr::SassInterpolated(value) => value.span.end,
        InterpolableStr::LessInterpolated(value) => value.span.end,
    }
}

fn import_href_end(href: &ImportPreludeHref<'_>) -> usize {
    match href {
        ImportPreludeHref::Str(value) => interpolable_str_end(value),
        ImportPreludeHref::Url(value) => value.span.end,
        ImportPreludeHref::Function(value) => value.span.end,
    }
}

fn import_media(source: &str, prelude: &ImportPrelude<'_>) -> String {
    if prelude.layer.is_some() || prelude.supports.is_some() || prelude.modifiers.is_some() {
        return source[import_href_end(&prelude.href)..prelude.span.end]
            .trim()
            .to_string();
    }
    prelude
        .media
        .as_ref()
        .map(|media| source[media.span.start..media.span.end].trim().to_string())
        .unwrap_or_default()
}

fn scope_escape_name(name: &InterpolableIdent<'_>) -> bool {
    matches!(
        name,
        InterpolableIdent::Literal(name)
            if matches!(name.name, "deep" | "global" | "slotted" | "v-deep" | "v-global" | "v-slotted")
    )
}

fn selector_is_unverifiable(selector: &oxc_css_parser::ast::ComplexSelector<'_>) -> bool {
    selector.children.iter().any(|child| {
        let ComplexSelectorChild::CompoundSelector(compound) = child else {
            return false;
        };
        compound.children.iter().any(|simple| match simple {
            SimpleSelector::Class(class) => !matches!(class.name, InterpolableIdent::Literal(_)),
            SimpleSelector::Id(id) => !matches!(id.name, InterpolableIdent::Literal(_)),
            SimpleSelector::Type(oxc_css_parser::ast::TypeSelector::TagName(tag)) => {
                !matches!(tag.name.name, InterpolableIdent::Literal(_))
            }
            SimpleSelector::Nesting(nesting) => nesting.suffix.is_some(),
            _ => false,
        })
    })
}

fn collect_compound_scope_escapes(
    compound: &CompoundSelector<'_>,
    source: &str,
    analysis: &mut StylesheetAnalysis,
) -> bool {
    let mut found = false;
    for simple in &compound.children {
        let escape_bounds = match simple {
            SimpleSelector::PseudoClass(pseudo) if scope_escape_name(&pseudo.name) => Some(
                pseudo
                    .arg
                    .as_ref()
                    .map(|arg| (arg.l_paren.end, arg.r_paren.start)),
            ),
            SimpleSelector::PseudoElement(pseudo) if scope_escape_name(&pseudo.name) => Some(
                pseudo
                    .arg
                    .as_ref()
                    .map(|arg| (arg.l_paren.end, arg.r_paren.start)),
            ),
            _ => None,
        };
        if let Some(bounds) = escape_bounds {
            found = true;
            if let Some((start, end)) = bounds {
                analysis
                    .scope_escapes
                    .push(format!("{} {{}}", &source[start..end]));
            } else {
                analysis.scope_escapes_unverifiable = true;
            }
            continue;
        }

        let SimpleSelector::PseudoClass(pseudo) = simple else {
            continue;
        };
        let Some(arg) = &pseudo.arg else { continue };
        match &arg.kind {
            PseudoClassSelectorArgKind::CompoundSelectorList(list) => {
                for selector in &list.selectors {
                    found |= collect_compound_scope_escapes(selector, source, analysis);
                }
            }
            PseudoClassSelectorArgKind::RelativeSelectorList(list) => {
                for selector in &list.selectors {
                    found |= collect_scope_escapes(&selector.complex_selector, source, analysis);
                }
            }
            PseudoClassSelectorArgKind::SelectorList(list) => {
                for selector in &list.selectors {
                    found |= collect_scope_escapes(selector, source, analysis);
                }
            }
            PseudoClassSelectorArgKind::Nth(nth) => {
                if let Some(list) = nth
                    .matcher
                    .as_ref()
                    .and_then(|matcher| matcher.selector.as_ref())
                {
                    for selector in &list.selectors {
                        found |= collect_scope_escapes(selector, source, analysis);
                    }
                }
            }
            _ => {}
        }
    }
    found
}

fn collect_compound_selector_classes(
    compound: &CompoundSelector<'_>,
    parent_classes: &[String],
    analysis: &mut StylesheetAnalysis,
) {
    for simple in &compound.children {
        match simple {
            SimpleSelector::Class(class) => match literal_ident(&class.name) {
                Some(name) => analysis.class_names.push(name.to_string()),
                None => analysis.class_reservations_unbounded = true,
            },
            // A suffixed parent selector compiles each parent class with
            // the suffix appended, so `.mr- { &auto {} }` reserves
            // `mr-auto`. A suffix with no class parents (or one the parser
            // cannot read) stays unbounded.
            SimpleSelector::Nesting(nesting) if nesting.suffix.is_some() => {
                match nesting.suffix.as_ref().and_then(static_ident_text) {
                    Some(suffix) if !parent_classes.is_empty() => {
                        for parent in parent_classes {
                            analysis.class_names.push(format!("{parent}{suffix}"));
                        }
                    }
                    _ => analysis.class_reservations_unbounded = true,
                }
            }
            // A nonliteral tag position interpolates arbitrary selector
            // text, which can be a class selector such as `.mr-auto`.
            SimpleSelector::Type(oxc_css_parser::ast::TypeSelector::TagName(tag))
                if !matches!(tag.name.name, InterpolableIdent::Literal(_)) =>
            {
                analysis.class_reservations_unbounded = true;
            }
            SimpleSelector::Attribute(attribute) => {
                let Some(name) = literal_ident(&attribute.name.name) else {
                    // An interpolated attribute name can resolve to
                    // `class`, so the match set cannot be bounded.
                    analysis.class_reservations_unbounded = true;
                    continue;
                };
                if name != "class" {
                    continue;
                }
                let value = attribute.value.as_ref().and_then(|value| match value {
                    AttributeSelectorValue::Ident(ident) => {
                        literal_ident(ident).map(str::to_string)
                    }
                    AttributeSelectorValue::Str(value) => literal_str(value),
                    _ => None,
                });
                let case_insensitive = attribute.modifier.is_some();
                match (attribute.matcher.as_ref().map(|matcher| &matcher.kind), value) {
                    // `[class]` alone matches presence, not a spelling.
                    (None, _) => {}
                    (
                        Some(
                            AttributeSelectorMatcherKind::Exact
                            | AttributeSelectorMatcherKind::MatchWord,
                        ),
                        Some(value),
                    ) => analysis.class_names.extend(value.split_whitespace().map(|word| {
                        // Canonical spellings are lowercase, so an
                        // ASCII-case-insensitive matcher reserves through
                        // its lowercase form.
                        if case_insensitive {
                            word.to_ascii_lowercase()
                        } else {
                            word.to_string()
                        }
                    })),
                    _ => analysis.class_reservations_unbounded = true,
                }
            }
            SimpleSelector::PseudoClass(pseudo) => {
                if let Some(arg) = &pseudo.arg {
                    collect_pseudo_arg_classes(&arg.kind, parent_classes, analysis);
                }
            }
            SimpleSelector::PseudoElement(pseudo) => {
                if let Some(arg) = &pseudo.arg {
                    match &arg.kind {
                        oxc_css_parser::ast::PseudoElementSelectorArgKind::CompoundSelector(
                            compound,
                        ) => collect_compound_selector_classes(compound, parent_classes, analysis),
                        oxc_css_parser::ast::PseudoElementSelectorArgKind::CompoundSelectorList(
                            list,
                        ) => {
                            for selector in &list.selectors {
                                collect_compound_selector_classes(
                                    selector,
                                    parent_classes,
                                    analysis,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_pseudo_arg_classes(
    kind: &PseudoClassSelectorArgKind<'_>,
    parent_classes: &[String],
    analysis: &mut StylesheetAnalysis,
) {
    match kind {
        PseudoClassSelectorArgKind::CompoundSelectorList(list) => {
            for selector in &list.selectors {
                collect_compound_selector_classes(selector, parent_classes, analysis);
            }
        }
        PseudoClassSelectorArgKind::RelativeSelectorList(list) => {
            for selector in &list.selectors {
                collect_selector_classes(&selector.complex_selector, parent_classes, analysis);
            }
        }
        PseudoClassSelectorArgKind::SelectorList(list) => {
            for selector in &list.selectors {
                collect_selector_classes(selector, parent_classes, analysis);
            }
        }
        PseudoClassSelectorArgKind::Nth(nth) => {
            if let Some(list) = nth.matcher.as_ref().and_then(|matcher| matcher.selector.as_ref()) {
                for selector in &list.selectors {
                    collect_selector_classes(selector, parent_classes, analysis);
                }
            }
        }
        _ => {}
    }
}

fn collect_selector_classes(
    selector: &oxc_css_parser::ast::ComplexSelector<'_>,
    parent_classes: &[String],
    analysis: &mut StylesheetAnalysis,
) {
    for child in &selector.children {
        if let ComplexSelectorChild::CompoundSelector(compound) = child {
            collect_compound_selector_classes(compound, parent_classes, analysis);
        }
    }
}

/// Literal class names of one selector list, resolved against the current
/// parents so chained suffix nesting keeps compounding.
fn selector_class_names(
    selectors: &oxc_css_parser::ast::SelectorList<'_>,
    parent_classes: &[String],
) -> Vec<String> {
    let mut names = Vec::new();
    for selector in &selectors.selectors {
        for child in &selector.children {
            if let ComplexSelectorChild::CompoundSelector(compound) = child {
                for simple in &compound.children {
                    match simple {
                        SimpleSelector::Class(class) => {
                            if let Some(name) = literal_ident(&class.name) {
                                names.push(name.to_string());
                            }
                        }
                        SimpleSelector::Nesting(nesting) if nesting.suffix.is_some() => {
                            if let Some(suffix) =
                                nesting.suffix.as_ref().and_then(static_ident_text)
                            {
                                for parent in parent_classes {
                                    names.push(format!("{parent}{suffix}"));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    names
}

fn collect_scope_escapes(
    selector: &oxc_css_parser::ast::ComplexSelector<'_>,
    source: &str,
    analysis: &mut StylesheetAnalysis,
) -> bool {
    let mut found = false;
    for child in &selector.children {
        match child {
            ComplexSelectorChild::Combinator(combinator)
                if matches!(
                    combinator.kind,
                    CombinatorKind::Deep
                        | CombinatorKind::ShadowChild
                        | CombinatorKind::ShadowDescendant
                ) =>
            {
                found = true;
                analysis.scope_escapes_unverifiable = true;
            }
            ComplexSelectorChild::Combinator(_) => {}
            ComplexSelectorChild::CompoundSelector(compound) => {
                found |= collect_compound_scope_escapes(compound, source, analysis);
            }
        }
    }
    found
}

/// The font-family descriptor value from parsed tokens only, normalized
/// the way families compare: case-insensitive with quoted and unquoted
/// spellings equivalent. Interpolated or otherwise nonliteral values
/// return None so the registration surface turns opaque.
fn font_family_identity(declaration: &oxc_css_parser::ast::Declaration<'_>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for value in &declaration.value {
        match value {
            ComponentValue::InterpolableStr(value) => parts.push(literal_str(value)?),
            ComponentValue::InterpolableIdent(ident) => {
                parts.push(literal_ident(ident)?.to_string());
            }
            _ => return None,
        }
    }
    Some(
        parts
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase(),
    )
}

fn declaration_value(source: &str, declaration: &oxc_css_parser::ast::Declaration<'_>) -> String {
    let Some(first) = declaration.value.first() else {
        return String::new();
    };
    let last = declaration.value.last().expect("checked");
    source[first.span().start..last.span().end]
        .trim()
        .to_string()
}

/// The static text of an ident, covering the plain literal form and a
/// Sass-interpolated ident whose elements are all static (the parser's
/// representation of a plain nesting suffix such as `&auto`).
fn static_ident_text(value: &InterpolableIdent<'_>) -> Option<String> {
    match value {
        InterpolableIdent::Literal(value) => Some(value.name.to_string()),
        InterpolableIdent::SassInterpolated(value) => value
            .elements
            .iter()
            .map(|element| match element {
                oxc_css_parser::ast::SassInterpolatedIdentElement::Static(part) => {
                    Some(part.value.to_string())
                }
                oxc_css_parser::ast::SassInterpolatedIdentElement::Expression(_) => None,
            })
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join("")),
        _ => None,
    }
}

fn literal_ident<'a>(value: &'a InterpolableIdent<'a>) -> Option<&'a str> {
    let InterpolableIdent::Literal(value) = value else {
        return None;
    };
    Some(value.name)
}

fn registration_prelude(prelude: Option<&AtRulePrelude<'_>>) -> Option<String> {
    match prelude? {
        AtRulePrelude::ColorProfile(ColorProfilePrelude::DashedIdent(value))
        | AtRulePrelude::CounterStyle(value)
        | AtRulePrelude::FontPaletteValues(value)
        | AtRulePrelude::PositionTry(value)
        | AtRulePrelude::Property(value) => literal_ident(value).map(str::to_string),
        AtRulePrelude::ColorProfile(ColorProfilePrelude::DeviceCmyk(value)) => {
            Some(value.name.to_string())
        }
        AtRulePrelude::FontFeatureValues(FontFamilyName::Str(value)) => literal_str(value),
        AtRulePrelude::FontFeatureValues(FontFamilyName::Unquoted(value)) => value
            .idents
            .iter()
            .map(literal_ident)
            .collect::<Option<Vec<_>>>()
            .map(|names| names.join(" ")),
        _ => None,
    }
}

fn collect_at_rule_metadata(
    statements: &[Statement<'_>],
    source: &str,
    syntax: Syntax,
    analysis: &mut StylesheetAnalysis,
) {
    for statement in statements {
        let Statement::AtRule(at_rule) = statement else {
            continue;
        };
        let Some(block) = &at_rule.block else {
            continue;
        };
        if at_rule.name.name == "theme" {
            for statement in &block.statements {
                let Statement::Declaration(declaration) = statement else {
                    continue;
                };
                let InterpolableIdent::Literal(name) = &declaration.name else {
                    continue;
                };
                if let Some(token) = name.name.strip_prefix("--") {
                    analysis
                        .theme_tokens
                        .insert(token.to_string(), declaration_value(source, declaration));
                }
            }
        }
        match at_rule.name.name {
            "font-face" => {
                let declaration = block.statements.iter().find_map(|statement| {
                    let Statement::Declaration(declaration) = statement else {
                        return None;
                    };
                    let InterpolableIdent::Literal(name) = &declaration.name else {
                        return None;
                    };
                    (name.name == "font-family").then_some(declaration)
                });
                match declaration.map(font_family_identity) {
                    Some(Some(family)) => analysis
                        .global_at_rule_identities
                        .push(format!("font-face {family}")),
                    // A nonliteral family can resolve to any name, so its
                    // collision surface is unknowable.
                    Some(None) => analysis.global_at_rules_unverifiable = true,
                    None => analysis
                        .global_at_rule_identities
                        .push("font-face ".to_string()),
                }
            }
            "page" => analysis.global_at_rule_identities.push("page".to_string()),
            name @ ("color-profile"
            | "counter-style"
            | "font-feature-values"
            | "font-palette-values"
            | "position-try"
            | "property"
            | "view-transition") => {
                if let Some(prelude) = registration_prelude(at_rule.prelude.as_ref()) {
                    analysis
                        .global_at_rule_identities
                        .push(format!("{name} {prelude}"));
                } else if syntax == Syntax::Css {
                    let prelude = source[at_rule.name.span.end..block.span.start].trim();
                    analysis
                        .global_at_rule_identities
                        .push(format!("{name} {prelude}"));
                } else {
                    analysis.global_at_rules_unverifiable = true;
                }
            }
            _ => {}
        }
        collect_at_rule_metadata(&block.statements, source, syntax, analysis);
    }
}

fn collect_stylesheet_statements(
    statements: &[Statement<'_>],
    source: &str,
    parent_classes: &[String],
    analysis: &mut StylesheetAnalysis,
) -> bool {
    let mut has_scope_escape = false;
    for statement in statements {
        match statement {
            Statement::AtRule(at_rule) => {
                let mut at_rule_parents: Option<Vec<String>> = None;
                match &at_rule.prelude {
                    // A `@utility` definition owns its class spelling in
                    // the built output, so a canonical alias must never
                    // adopt it; a functional `name-*` utility owns an
                    // unbounded family of spellings.
                    _ if at_rule.name.name == "utility" => {
                        let prelude = at_rule
                            .block
                            .as_ref()
                            .map(|block| source[at_rule.span.start..block.span.start].trim())
                            .and_then(|text| text.strip_prefix("@utility"))
                            .map(str::trim)
                            .unwrap_or("");
                        if prelude.is_empty() || prelude.ends_with("-*") {
                            analysis.class_reservations_unbounded = true;
                        } else {
                            analysis.class_names.push(prelude.to_string());
                        }
                    }
                    // A selector-prelude `@at-root` emits its rule at the
                    // stylesheet root, so its classes reserve like rule
                    // selectors and parent nested rules resolve against it.
                    Some(AtRulePrelude::SassAtRoot(prelude)) => {
                        if let oxc_css_parser::ast::SassAtRootKind::Selector(list) = &prelude.kind {
                            for selector in &list.selectors {
                                collect_selector_classes(selector, parent_classes, analysis);
                            }
                            at_rule_parents = Some(selector_class_names(list, parent_classes));
                        }
                    }
                    // `@scope` roots and limits match live elements, so
                    // their selectors reserve spellings like rule selectors.
                    Some(AtRulePrelude::Scope(prelude)) => {
                        let (start, end) = match prelude.as_ref() {
                            oxc_css_parser::ast::ScopePrelude::StartOnly(start) => {
                                (start.selector.as_ref(), None)
                            }
                            oxc_css_parser::ast::ScopePrelude::EndOnly(end) => {
                                (None, end.selector.as_ref())
                            }
                            oxc_css_parser::ast::ScopePrelude::Both(both) => {
                                (both.start.selector.as_ref(), both.end.selector.as_ref())
                            }
                        };
                        for list in [start, end].into_iter().flatten() {
                            for selector in &list.selectors {
                                collect_selector_classes(selector, parent_classes, analysis);
                            }
                        }
                    }
                    Some(AtRulePrelude::Import(prelude)) => match import_href(&prelude.href) {
                        Some(reference) => analysis.references.push(reference),
                        None => analysis.unverifiable = true,
                    },
                    Some(AtRulePrelude::LessImport(prelude)) => match import_href(&prelude.href) {
                        Some(reference) => analysis.references.push(reference),
                        None => analysis.unverifiable = true,
                    },
                    Some(AtRulePrelude::SassImport(prelude)) => {
                        analysis
                            .references
                            .extend(prelude.paths.iter().map(|path| path.value.to_string()));
                    }
                    Some(AtRulePrelude::SassUse(prelude)) => match literal_str(&prelude.path) {
                        Some(reference) => analysis.references.push(reference),
                        None => analysis.unverifiable = true,
                    },
                    Some(AtRulePrelude::SassForward(prelude)) => match literal_str(&prelude.path) {
                        Some(reference) => analysis.references.push(reference),
                        None => analysis.unverifiable = true,
                    },
                    _ => {}
                }
                if let Some(block) = &at_rule.block {
                    has_scope_escape |= collect_stylesheet_statements(
                        &block.statements,
                        source,
                        at_rule_parents.as_deref().unwrap_or(parent_classes),
                        analysis,
                    );
                }
            }
            Statement::QualifiedRule(rule) => {
                let mut direct = false;
                for selector in &rule.selector.selectors {
                    direct |= collect_scope_escapes(selector, source, analysis);
                    collect_selector_classes(selector, parent_classes, analysis);
                    if selector_is_unverifiable(selector) {
                        analysis.selectors_unverifiable = true;
                    }
                }
                let nested = collect_stylesheet_statements(
                    &rule.block.statements,
                    source,
                    &selector_class_names(&rule.selector, parent_classes),
                    analysis,
                );
                if nested {
                    // Reconstructing mixed declaration and nested-rule scope is
                    // outside this collector; retain conservatively.
                    analysis.scope_escapes_unverifiable = true;
                }
                if !direct && !nested {
                    analysis.scope_shadow_css.push(format!(
                        "{} {{}}",
                        &source[rule.selector.span.start..rule.selector.span.end]
                    ));
                }
                has_scope_escape |= direct || nested;
            }
            Statement::Declaration(declaration) => {
                let InterpolableIdent::Literal(name) = &declaration.name else {
                    continue;
                };
                if name.name != "composes" {
                    continue;
                }
                let mut from = false;
                let mut reference_read = false;
                for value in &declaration.value {
                    if from {
                        match value {
                            ComponentValue::InterpolableStr(value) => match literal_str(value) {
                                Some(reference) => {
                                    analysis.references.push(reference);
                                    reference_read = true;
                                }
                                None => analysis.unverifiable = true,
                            },
                            ComponentValue::InterpolableIdent(InterpolableIdent::Literal(
                                ident,
                            )) if ident.name == "global" => {
                                reference_read = true;
                            }
                            _ => analysis.unverifiable = true,
                        }
                        break;
                    }
                    if matches!(value, ComponentValue::InterpolableIdent(InterpolableIdent::Literal(ident)) if ident.name == "from")
                    {
                        from = true;
                    }
                }
                if from && !reference_read {
                    analysis.unverifiable = true;
                }
            }
            _ => {}
        }
    }
    has_scope_escape
}

fn collect_loading_imports(
    statements: &[Statement<'_>],
    source: &str,
    analysis: &mut StylesheetAnalysis,
) {
    let mut imports_allowed = true;
    for statement in statements {
        let Statement::AtRule(at_rule) = statement else {
            imports_allowed = false;
            continue;
        };
        if let Some(AtRulePrelude::Import(prelude)) = &at_rule.prelude {
            if imports_allowed {
                if let Some(href) = import_href(&prelude.href) {
                    let end = at_rule.span.end
                        + usize::from(source.as_bytes().get(at_rule.span.end) == Some(&b';'));
                    analysis.imports.push(StylesheetImport {
                        href,
                        media: import_media(source, prelude),
                        start: at_rule.span.start,
                        end,
                    });
                } else {
                    analysis.unverifiable = true;
                }
            }
        } else if at_rule.block.is_some() {
            imports_allowed = false;
        }
    }
}

pub fn stylesheet_analysis_json(path: &str, source: &str) -> MigrationResult<String> {
    let syntax =
        stylesheet_syntax(path).map_err(|message| MigrationError::UnsupportedSource { message })?;
    let allocator = oxc_css_parser::Allocator::default();
    let parsed = parse_css(&allocator, source, syntax).map_err(|error| {
        MigrationError::AuthoredStylesheetParse {
            message: format!("Failed to parse {path}: {error}"),
        }
    })?;
    let mut analysis = StylesheetAnalysis {
        references: Vec::new(),
        imports: Vec::new(),
        unverifiable: false,
        scope_escapes: Vec::new(),
        scope_shadow_css: Vec::new(),
        scope_escapes_unverifiable: false,
        selectors_unverifiable: false,
        theme_tokens: BTreeMap::new(),
        global_at_rule_identities: Vec::new(),
        global_at_rules_unverifiable: false,
        class_names: Vec::new(),
        class_reservations_unbounded: false,
    };
    collect_stylesheet_statements(&parsed.statements, source, &[], &mut analysis);
    collect_at_rule_metadata(&parsed.statements, source, syntax, &mut analysis);
    if syntax == Syntax::Css {
        collect_loading_imports(&parsed.statements, source, &mut analysis);
    }
    analysis.references.sort();
    analysis.references.dedup();
    analysis.class_names.sort();
    analysis.class_names.dedup();
    serde_json::to_string(&analysis).map_err(|error| MigrationError::Serialization {
        message: error.to_string(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompiledShape {
    declarations: Vec<CompiledDeclaration>,
    referenced_properties: Vec<String>,
}

#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
struct CompiledDeclaration {
    property: String,
    important: bool,
}

fn collect_value_references(values: &[ComponentValue<'_>], references: &mut Vec<String>) {
    for value in values {
        match value {
            ComponentValue::Function(function) => {
                if matches!(
                    &function.name,
                    oxc_css_parser::ast::FunctionName::Ident(InterpolableIdent::Literal(name))
                        if name.name == "var"
                ) && let Some(ComponentValue::InterpolableIdent(InterpolableIdent::Literal(
                    ident,
                ))) = function.args.first()
                    && ident.name.starts_with("--")
                {
                    references.push(ident.name.to_string());
                }
                collect_value_references(&function.args, references);
            }
            ComponentValue::Calc(calc) => {
                collect_value_references(std::slice::from_ref(&calc.left), references);
                collect_value_references(std::slice::from_ref(&calc.right), references);
            }
            _ => {}
        }
    }
}

fn collect_shape_statements(
    statements: &[Statement<'_>],
    shape: &mut CompiledShape,
) -> Result<(), String> {
    for statement in statements {
        match statement {
            Statement::Declaration(declaration) => {
                let InterpolableIdent::Literal(name) = &declaration.name else {
                    return Err("Nonliteral compiled property".to_string());
                };
                shape.declarations.push(CompiledDeclaration {
                    property: name.name.to_string(),
                    important: declaration.important.is_some(),
                });
                collect_value_references(&declaration.value, &mut shape.referenced_properties);
            }
            Statement::QualifiedRule(rule) => {
                collect_shape_statements(&rule.block.statements, shape)?;
            }
            Statement::AtRule(at_rule) => {
                if let Some(block) = &at_rule.block {
                    collect_shape_statements(&block.statements, shape)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The declaration shape of one compiled Tailwind utility: every effective
/// property with its importance, in sorted order, plus every custom
/// property the compiled values dereference. The orchestration layer
/// compares shapes to reject canonical aliases with companion declarations
/// and to prove literal runtime stability.
pub fn compiled_shape_json(css: &str) -> MigrationResult<String> {
    let allocator = oxc_css_parser::Allocator::default();
    let parsed = parse_css(&allocator, css, Syntax::Css).map_err(|error| {
        MigrationError::EditedStylesheetParse {
            message: format!("Failed to parse compiled CSS: {error}"),
        }
    })?;
    let mut shape = CompiledShape {
        declarations: Vec::new(),
        referenced_properties: Vec::new(),
    };
    collect_shape_statements(&parsed.statements, &mut shape)
        .map_err(|message| MigrationError::EditedStylesheetParse { message })?;
    shape.declarations.sort();
    shape.referenced_properties.sort();
    shape.referenced_properties.dedup();
    serde_json::to_string(&shape).map_err(|error| MigrationError::Serialization {
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn collects_stylesheet_dependencies_for_every_supported_syntax() {
        for (path, source, expected) in [
            (
                "/p/main.css",
                "@import \"./base.css\" screen;\n.x { composes: y from \"./x.module.css\"; }\n.global { composes: reset from global; }\n",
                vec!["./base.css", "./x.module.css"],
            ),
            (
                "/p/main.scss",
                "@use \"./tokens\";\n@forward \"./shared\";\n@import \"./legacy\";\n.x { composes: y from \"./x.module.css\"; }\n",
                vec!["./legacy", "./shared", "./tokens", "./x.module.css"],
            ),
            (
                "/p/main.sass",
                "@use \"./tokens\"\n@forward \"./shared\"\n@import \"./legacy\"\n.x\n  composes: y from \"./x.module.css\"\n",
                vec!["./legacy", "./shared", "./tokens", "./x.module.css"],
            ),
            (
                "/p/main.less",
                "@import (reference) \"./tokens.less\";\n.x { composes: y from \"./x.module.css\"; }\n",
                vec!["./tokens.less", "./x.module.css"],
            ),
        ] {
            let parsed: serde_json::Value =
                serde_json::from_str(&super::stylesheet_analysis_json(path, source).unwrap())
                    .unwrap();
            assert_eq!(parsed["references"], serde_json::json!(expected), "{path}");
            assert_eq!(parsed["unverifiable"], false, "{path}");
        }
    }

    #[test]
    fn collects_loading_import_media_and_byte_spans() {
        let source =
            "/* 😀 */\n@import url(\"./print.css\") print;\n.rule {}\n@import \"./late.css\";\n";
        let parsed: serde_json::Value =
            serde_json::from_str(&super::stylesheet_analysis_json("/p/main.css", source).unwrap())
                .unwrap();
        let start = source.find("@import").unwrap();
        let end = source[start..].find(';').unwrap() + start + 1;
        assert_eq!(
            parsed["imports"],
            serde_json::json!([{
                "href": "./print.css",
                "media": "print",
                "start": start,
                "end": end
            }])
        );
    }

    #[test]
    fn collects_conditional_loading_import_modifiers() {
        let source = "@import \"./layered.css\" layer(base);\n@import \"./grid.css\" supports(display: grid);\n";
        let parsed: serde_json::Value =
            serde_json::from_str(&super::stylesheet_analysis_json("/p/main.css", source).unwrap())
                .unwrap();
        assert_eq!(parsed["imports"][0]["media"], "layer(base)");
        assert_eq!(parsed["imports"][1]["media"], "supports(display: grid)");
    }

    #[test]
    fn collects_theme_tokens_and_global_at_rule_identities() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/theme.css",
                "@theme { --spacing-card: calc(1rem + 2px); }\n\
                 @font-face { font-family: \"My  Font\"; src: url(font.woff2); }\n\
                 @media print { @font-face { font-family: Print; src: url(print.woff2); } }\n\
                 @property --angle { syntax: \"<angle>\"; }\n\
                 @property /* docs */ --angle { syntax: \"<angle>\"; }\n\
                 @page :left { margin: 1cm; }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["themeTokens"],
            serde_json::json!({ "spacing-card": "calc(1rem + 2px)" })
        );
        assert_eq!(
            parsed["globalAtRuleIdentities"],
            serde_json::json!([
                "font-face my font",
                "font-face print",
                "property --angle",
                "property --angle",
                "page"
            ])
        );
    }

    #[test]
    fn collects_vue_scope_escapes_from_selector_nodes() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/scoped.css",
                ".a :deep(.inside:is(.x, .y)) {}\n.b::v-global(.free) {}\n.c :global .open {}\n.d /deep/ .legacy {}\n.e :deep(.host):is(:deep(.card), :not(:global(.note))) {}\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["scopeEscapes"],
            serde_json::json!([
                ".inside:is(.x, .y) {}",
                ".free {}",
                ".host {}",
                ".card {}",
                ".note {}"
            ])
        );
        assert_eq!(parsed["scopeEscapesUnverifiable"], true);
        assert_eq!(parsed["selectorsUnverifiable"], false);
    }

    #[test]
    fn collects_authored_class_reservations() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/legacy.css",
                ".mr-auto { margin-right: auto; }\n.card:is(.p-4, .stack) {}\n[class~=\"font-open-sans\"] { color: red; }\n[class=\"pill wide\"] {}\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["classNames"],
            serde_json::json!(["card", "font-open-sans", "mr-auto", "p-4", "pill", "stack", "wide"])
        );
        assert_eq!(parsed["classReservationsUnbounded"], false);
    }

    #[test]
    fn reserves_case_insensitive_and_scope_prelude_classes() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/legacy.css",
                "[class~=\"MR-AUTO\" i] { color: red; }\n@scope (.p-4) to (.stack) { .legacy { color: blue; } }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed["classNames"],
            serde_json::json!(["legacy", "mr-auto", "p-4", "stack"])
        );
        assert_eq!(parsed["classReservationsUnbounded"], false);
    }

    #[test]
    fn resolves_suffixed_nesting_selectors_against_parents() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/legacy.scss",
                ".mr- { &auto { color: red; } }\n.btn { &-primary { color: blue; } }\n",
            )
            .unwrap(),
        )
        .unwrap();
        let names = parsed["classNames"].as_array().unwrap();
        assert!(names.contains(&serde_json::json!("mr-auto")), "{parsed}");
        assert!(names.contains(&serde_json::json!("btn-primary")), "{names:?}");
        assert_eq!(parsed["classReservationsUnbounded"], false);
    }

    #[test]
    fn marks_unbounded_class_attribute_matchers() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json("/p/legacy.css", "[class*=\"auto\"] { color: red; }\n")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["classReservationsUnbounded"], true);
    }

    #[test]
    fn reserves_entry_utility_definitions() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/globals.css",
                "@utility mr-auto {\n  margin-right: 1px;\n}\n.card { color: red; }\n",
            )
            .unwrap(),
        )
        .unwrap();
        let names = parsed["classNames"].as_array().unwrap();
        assert!(names.contains(&serde_json::json!("mr-auto")), "{names:?}");
        assert_eq!(parsed["classReservationsUnbounded"], false);

        // A functional utility owns every spelling under its prefix.
        let functional: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/globals.css",
                "@utility tab-* {\n  tab-size: --value(integer);\n}\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(functional["classReservationsUnbounded"], true);
    }

    #[test]
    fn reserves_sass_at_root_prelude_selectors() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/legacy.scss",
                ".card { @at-root .mr-auto { color: red; } }\n.mr- { @at-root .top { &auto { color: blue; } } }\n",
            )
            .unwrap(),
        )
        .unwrap();
        let names = parsed["classNames"].as_array().unwrap();
        assert!(names.contains(&serde_json::json!("mr-auto")), "{names:?}");
        // Nested rules resolve their parent against the at-root selector.
        assert!(names.contains(&serde_json::json!("topauto")), "{names:?}");
        assert_eq!(parsed["classReservationsUnbounded"], false);
    }

    #[test]
    fn marks_unbounded_interpolated_selector_positions() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/legacy.scss",
                "$sel: \".mr-auto\";\n#{$sel} { color: red; }\ndiv { color: blue; }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["classReservationsUnbounded"], true);

        let literal: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json("/p/plain.css", "div { color: blue; }\n").unwrap(),
        )
        .unwrap();
        assert_eq!(literal["classReservationsUnbounded"], false);
    }

    #[test]
    fn marks_unbounded_interpolated_attribute_names() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/legacy.scss",
                "$attr: class;\n[#{$attr}~=\"mr-auto\"] { color: red; }\n[data-kind=\"x\"] { color: blue; }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["classReservationsUnbounded"], true);
    }

    #[test]
    fn extracts_compiled_declaration_shapes() {
        let shape: serde_json::Value = serde_json::from_str(
            &super::compiled_shape_json(
                ".mr-auto { margin-right: auto; }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            shape,
            serde_json::json!({
                "declarations": [{ "property": "margin-right", "important": false }],
                "referencedProperties": [],
            })
        );

        let themed: serde_json::Value = serde_json::from_str(
            &super::compiled_shape_json(
                ".p-4 { padding: calc(var(--spacing) * 4); }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(themed["referencedProperties"], serde_json::json!(["--spacing"]));

        let important: serde_json::Value = serde_json::from_str(
            &super::compiled_shape_json(
                "@media (hover: hover) { .hover\\:text-sm:hover { font-size: 1rem !important; line-height: 1.5; } }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            shape["declarations"] != important["declarations"],
            true,
        );
        assert_eq!(
            important["declarations"],
            serde_json::json!([
                { "property": "font-size", "important": true },
                { "property": "line-height", "important": false },
            ])
        );
    }

    #[test]
    fn marks_interpolated_font_families_unverifiable() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/main.scss",
                "$family: Brand;\n@font-face { font-family: #{$family}; src: url(a.woff2); }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["globalAtRuleIdentities"], serde_json::json!([]));
        assert_eq!(parsed["globalAtRulesUnverifiable"], true);
    }

    #[test]
    fn marks_interpolated_registration_identities_unverifiable() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/main.scss",
                "$name: brand;\n@property --#{$name} { syntax: \"<color>\"; }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["globalAtRuleIdentities"], serde_json::json!([]));
        assert_eq!(parsed["globalAtRulesUnverifiable"], true);
    }

    #[test]
    fn marks_interpolated_selectors_unverifiable() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/main.scss",
                "$kind: card;\n.#{$kind} {}\n.block { &-active {} }\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["selectorsUnverifiable"], true);
    }

    #[test]
    fn marks_interpolated_stylesheet_dependencies_unverifiable() {
        let parsed: serde_json::Value = serde_json::from_str(
            &super::stylesheet_analysis_json(
                "/p/main.scss",
                "$name: \"theme\";\n@use \"./#{$name}\";\n",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["references"], serde_json::json!([]));
        assert_eq!(parsed["unverifiable"], true);
    }
}
