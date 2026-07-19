use std::{
    collections::{BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Expression, ImportDeclaration, ImportDeclarationSpecifier, JSXAttributeItem, JSXAttributeName,
    JSXAttributeValue, JSXExpression, JSXOpeningElement, StaticMemberExpression, TemplateLiteral,
};
use oxc_ast_visit::{Visit, walk};
use oxc_css_parser::{
    Parser as CssParser, Syntax,
    ast::{ComplexSelectorChild, InterpolableIdent, SimpleSelector, Statement, Stylesheet},
};
use oxc_parser::Parser;
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::{SourceType, Span};
use oxc_syntax::symbol::SymbolId;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanRequest {
    css_path: String,
    css_source: String,
    #[serde(default)]
    theme_tokens: HashMap<String, String>,
    files: Vec<SourceFile>,
}

#[derive(Deserialize)]
struct SourceFile {
    path: String,
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanResponse {
    files: Vec<PlannedFile>,
    candidates: Vec<String>,
    converted_rules: usize,
    retained_rules: usize,
    rules: Vec<RuleReport>,
    warnings: Vec<Warning>,
}

#[derive(Serialize)]
struct PlannedFile {
    path: String,
    source: String,
}

#[derive(Serialize)]
struct RuleReport {
    selector: String,
    status: &'static str,
    candidates: Vec<String>,
}

#[derive(Serialize)]
struct Warning {
    code: &'static str,
    file: String,
    start: usize,
    end: usize,
    message: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SelectorKey {
    Class(String),
    Id(String),
}

#[derive(Clone)]
struct RulePlan {
    span: std::ops::Range<usize>,
    selector: String,
    related_classes: Vec<String>,
    key: Option<SelectorKey>,
    candidates: Vec<String>,
    warning: Option<&'static str>,
}

#[derive(Clone)]
struct Edit {
    start: usize,
    end: usize,
    replacement: String,
}

pub fn plan_json(request: &str) -> Result<String, String> {
    let request: PlanRequest = serde_json::from_str(request).map_err(|error| error.to_string())?;
    let is_module = request.css_path.ends_with(".module.css");
    let rules = parse_css_rules(
        &request.css_path,
        &request.css_source,
        &request.theme_tokens,
        is_module,
    )?;

    let blocked_classes = rules
        .iter()
        .filter(|rule| rule.warning.is_some())
        .flat_map(|rule| rule.related_classes.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut candidate_map: HashMap<SelectorKey, Vec<String>> = HashMap::new();
    for rule in &rules {
        if let Some(key) = &rule.key
            && rule.warning.is_none()
            && !matches!(key, SelectorKey::Class(name) if blocked_classes.contains(name))
        {
            candidate_map
                .entry(key.clone())
                .or_default()
                .extend(rule.candidates.clone());
        }
    }
    for candidates in candidate_map.values_mut() {
        candidates.sort();
        candidates.dedup();
    }

    let mut planned_files = Vec::new();
    let mut candidates = BTreeSet::new();
    let mut module_refs: HashMap<String, usize> = HashMap::new();
    let mut matched_module_refs: HashMap<String, usize> = HashMap::new();
    let mut warnings = Vec::new();

    for file in &request.files {
        let result = plan_source_file(file, &request.css_path, is_module, &candidate_map)?;

        for candidate in result.candidates {
            candidates.insert(candidate);
        }
        merge_counts(&mut module_refs, result.module_refs);
        merge_counts(&mut matched_module_refs, result.matched_module_refs);
        warnings.extend(result.warnings);

        if !result.edits.is_empty() {
            let source = apply_edits(&file.source, result.edits)?;
            validate_js(&file.path, &source)?;
            planned_files.push(PlannedFile {
                path: file.path.clone(),
                source,
            });
        }
    }

    let mut css_edits = Vec::new();
    let mut converted_rules = 0;
    let mut retained_rules = 0;
    let mut rule_reports = Vec::new();

    for rule in rules {
        let can_remove = is_module
            && rule.warning.is_none()
            && match &rule.key {
                Some(SelectorKey::Class(name)) => {
                    let refs = module_refs.get(name).copied().unwrap_or(0);
                    refs > 0 && matched_module_refs.get(name).copied().unwrap_or(0) == refs
                }
                _ => false,
            };

        if can_remove {
            converted_rules += 1;
            css_edits.push(Edit {
                start: rule.span.start,
                end: rule.span.end,
                replacement: String::new(),
            });
            rule_reports.push(RuleReport {
                selector: rule.selector,
                status: "converted",
                candidates: rule.candidates,
            });
        } else {
            retained_rules += 1;
            let (code, message) = if let Some(code) = rule.warning {
                (
                    code,
                    "The rule is outside the supported declaration or selector subset.".to_string(),
                )
            } else if !is_module {
                (
                    "retained-global-rule",
                    "Global CSS is never deleted automatically.".to_string(),
                )
            } else {
                (
                    "unresolved-selector-target",
                    "No exclusively supported className references were found.".to_string(),
                )
            };
            warnings.push(Warning {
                code,
                file: request.css_path.clone(),
                start: rule.span.start,
                end: rule.span.end,
                message,
            });
            rule_reports.push(RuleReport {
                selector: rule.selector,
                status: "retained",
                candidates: rule.candidates,
            });
        }
    }

    if !css_edits.is_empty() {
        let source = apply_edits(&request.css_source, css_edits)?;
        validate_css(&source)?;
        planned_files.push(PlannedFile {
            path: request.css_path,
            source,
        });
    }

    serde_json::to_string(&PlanResponse {
        files: planned_files,
        candidates: candidates.into_iter().collect(),
        converted_rules,
        retained_rules,
        rules: rule_reports,
        warnings,
    })
    .map_err(|error| error.to_string())
}

fn merge_counts(target: &mut HashMap<String, usize>, source: HashMap<String, usize>) {
    for (key, count) in source {
        *target.entry(key).or_default() += count;
    }
}

#[derive(Default)]
struct SpacingValues {
    values: [Option<String>; 4],
    used: bool,
}

impl SpacingValues {
    fn apply(&mut self, property: &str, family: &str, value: &str) -> Result<bool, ()> {
        if property == family {
            let parts = value.split_whitespace().collect::<Vec<_>>();
            let sides = match parts.as_slice() {
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

    fn candidates(
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

fn parse_css_rules(
    path: &str,
    source: &str,
    theme_tokens: &HashMap<String, String>,
    is_module: bool,
) -> Result<Vec<RulePlan>, String> {
    let allocator = oxc_css_parser::Allocator::default();
    let mut parser = CssParser::new(&allocator, source, Syntax::Css);
    let stylesheet = parser
        .parse::<Stylesheet>()
        .map_err(|error| format!("Failed to parse {path}: {error:?}"))?;

    let mut composed_classes = BTreeSet::new();
    if is_module {
        for statement in &stylesheet.statements {
            let Statement::QualifiedRule(rule) = statement else {
                continue;
            };
            for statement in &rule.block.statements {
                let Statement::Declaration(declaration) = statement else {
                    continue;
                };
                if literal_ident(&declaration.name) == Some("composes") {
                    let value = declaration_value(source, declaration);
                    for class in value.split_whitespace().take_while(|part| *part != "from") {
                        composed_classes.insert(class.to_string());
                    }
                }
            }
        }
    }

    let mut qualified_rules = Vec::new();
    let mut rules = Vec::new();
    for statement in &stylesheet.statements {
        match statement {
            Statement::QualifiedRule(rule) => qualified_rules.push((rule, None)),
            Statement::AtRule(at_rule) if at_rule.name.name == "media" => {
                let Some(variant) = media_breakpoint_variant(at_rule, source, theme_tokens) else {
                    rules.push(RulePlan {
                        span: at_rule.span.start..at_rule.span.end,
                        selector: source[at_rule.span.start
                            ..at_rule
                                .block
                                .as_ref()
                                .map_or(at_rule.span.end, |block| block.span.start)]
                            .trim()
                            .to_string(),
                        related_classes: Vec::new(),
                        key: None,
                        candidates: Vec::new(),
                        warning: Some("unsupported-media-query"),
                    });
                    continue;
                };
                if let Some(block) = &at_rule.block {
                    for statement in &block.statements {
                        if let Statement::QualifiedRule(rule) = statement {
                            qualified_rules.push((rule, Some(variant.clone())));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for (rule, outer_variant) in qualified_rules {
        let selector = source[rule.selector.span.start..rule.selector.span.end].to_string();
        let selector_match = selector_match(rule, source, is_module);
        let key = selector_match.as_ref().map(|(key, _)| key.clone());
        let variant = match (
            outer_variant,
            selector_match.and_then(|(_, variant)| variant),
        ) {
            (Some(outer), Some(inner)) => Some(format!("{outer}:{inner}")),
            (Some(outer), None) => Some(outer),
            (None, inner) => inner,
        };
        let mut candidates = Vec::new();
        let mut margin = SpacingValues::default();
        let mut padding = SpacingValues::default();
        let mut warning = key.is_none().then_some("unsupported-selector");
        if matches!(&key, Some(SelectorKey::Class(name)) if composed_classes.contains(name)) {
            warning = Some("css-module-composes");
        }

        for statement in &rule.block.statements {
            let Statement::Declaration(declaration) = statement else {
                warning = Some("unsupported-rule-content");
                continue;
            };
            if declaration.important.is_some() {
                warning = Some("unsupported-important");
                continue;
            }
            let Some(property) = literal_ident(&declaration.name) else {
                warning = Some("unsupported-declaration");
                continue;
            };
            let value = declaration_value(source, declaration);
            if property == "composes" {
                warning = Some("css-module-composes");
                continue;
            }
            let spacing_result = margin.apply(property, "margin", value).and_then(|handled| {
                if handled {
                    Ok(true)
                } else {
                    padding.apply(property, "padding", value)
                }
            });
            match spacing_result {
                Ok(true) => continue,
                Err(()) => {
                    warning = Some("unsupported-overlap");
                    continue;
                }
                Ok(false) => {}
            }
            match declaration_to_candidate(property, value, theme_tokens) {
                Some(candidate) => candidates.push(candidate),
                None => warning = Some("unsupported-declaration"),
            }
        }

        candidates.extend(margin.candidates("m", theme_tokens));
        candidates.extend(padding.candidates("p", theme_tokens));
        if candidates.is_empty() {
            warning = Some("unsupported-declaration");
        }
        if let Some(variant) = variant {
            for candidate in &mut candidates {
                *candidate = format!("{variant}:{candidate}");
            }
        }
        candidates.sort();
        candidates.dedup();
        rules.push(RulePlan {
            span: rule.span.start..rule.span.end,
            selector,
            related_classes: selector_classes(rule),
            key,
            candidates,
            warning,
        });
    }
    Ok(rules)
}

fn selector_classes(rule: &oxc_css_parser::ast::QualifiedRule<'_>) -> Vec<String> {
    rule.selector
        .selectors
        .iter()
        .flat_map(|selector| &selector.children)
        .filter_map(|child| match child {
            ComplexSelectorChild::CompoundSelector(compound) => Some(compound),
            ComplexSelectorChild::Combinator(_) => None,
        })
        .flat_map(|compound| &compound.children)
        .filter_map(|selector| match selector {
            SimpleSelector::Class(class) => literal_ident(&class.name).map(str::to_string),
            _ => None,
        })
        .collect()
}

fn media_breakpoint_variant(
    at_rule: &oxc_css_parser::ast::AtRule<'_>,
    source: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    let block_start = at_rule.block.as_ref()?.span.start;
    let query = &source[at_rule.span.start..block_start];
    let value_start = query.find("(min-width:")? + "(min-width:".len();
    let value_end = query[value_start..].find(')')? + value_start;
    let value = query[value_start..value_end].trim();
    let mut matches = theme_tokens
        .iter()
        .filter(|(name, token_value)| {
            name.starts_with("breakpoint-") && token_value.trim() == value
        })
        .map(|(name, _)| name["breakpoint-".len()..].to_string())
        .collect::<Vec<_>>();
    matches.sort();
    matches.into_iter().next()
}

fn declaration_value<'a>(
    source: &'a str,
    declaration: &oxc_css_parser::ast::Declaration<'_>,
) -> &'a str {
    source[declaration.colon_span.end..declaration.span.end]
        .trim()
        .trim_end_matches(';')
        .trim()
}

fn selector_match(
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
        let key = selector_key(compound.children.first()?)?;
        let variant = match compound.children.as_slice() {
            [_] => None,
            [_, SimpleSelector::PseudoClass(pseudo)] if pseudo.arg.is_none() => {
                let name = literal_ident(&pseudo.name)?;
                if !matches!(
                    name,
                    "active"
                        | "disabled"
                        | "focus"
                        | "focus-visible"
                        | "focus-within"
                        | "hover"
                        | "visited"
                ) {
                    return None;
                }
                Some(name.to_string())
            }
            _ if !is_module => arbitrary_selector_variant(rule, source, compound)?,
            _ => return None,
        };
        return Some((key, variant));
    }

    if is_module {
        return None;
    }
    let ComplexSelectorChild::CompoundSelector(target) = selector.children.last()? else {
        return None;
    };
    let key = selector_key(target.children.first()?)?;
    let variant = arbitrary_selector_variant(rule, source, target)?;
    Some((key, variant))
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
) -> Option<Option<String>> {
    let key = selector_key(target.children.first()?)?;
    let anchor = match key {
        SelectorKey::Class(name) => format!(".{name}"),
        SelectorKey::Id(name) => format!("#{name}"),
    };
    let selector = &source[rule.selector.span.start..rule.selector.span.end];
    let index = selector.rfind(&anchor)?;
    let mut condition = selector.to_string();
    condition.replace_range(index..index + anchor.len(), "&");
    Some(Some(format!(
        "[{}]",
        condition.split_whitespace().collect::<Vec<_>>().join("_")
    )))
}

fn literal_ident<'a>(ident: &'a InterpolableIdent<'a>) -> Option<&'a str> {
    match ident {
        InterpolableIdent::Literal(ident) => Some(ident.name),
        _ => None,
    }
}

fn declaration_to_candidate(
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

fn exact_theme_token(
    namespace: &str,
    value: &str,
    theme_tokens: &HashMap<String, String>,
) -> Option<String> {
    let token_prefix = format!("{namespace}-");
    let mut named = theme_tokens
        .iter()
        .filter(|(name, token_value)| {
            name.starts_with(&token_prefix) && token_value.trim() == value
        })
        .map(|(name, _)| name[token_prefix.len()..].to_string())
        .collect::<Vec<_>>();
    named.sort();
    if let Some(name) = named.into_iter().next() {
        return Some(name);
    }

    if namespace == "spacing"
        && let Some(base) = theme_tokens.get("spacing")
        && let (Some((value_number, value_unit)), Some((base_number, base_unit))) =
            (parse_dimension(value), parse_dimension(base))
        && value_unit == base_unit
        && base_number != 0.0
    {
        let multiplier = value_number / base_number;
        if multiplier.is_finite() && multiplier >= 0.0 {
            return Some(format_number(multiplier));
        }
    }
    None
}

fn parse_dimension(value: &str) -> Option<(f64, &str)> {
    let split = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii_digit() && !matches!(character, '.' | '-'))
        .map(|(index, _)| index)?;
    let (number, unit) = value.split_at(split);
    Some((number.parse().ok()?, unit))
}

fn format_number(number: f64) -> String {
    if number.fract() == 0.0 {
        format!("{number:.0}")
    } else {
        number.to_string()
    }
}

fn arbitrary_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join("_")
}

struct SourcePlan {
    edits: Vec<Edit>,
    candidates: Vec<String>,
    module_refs: HashMap<String, usize>,
    matched_module_refs: HashMap<String, usize>,
    warnings: Vec<Warning>,
}

fn plan_source_file(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
) -> Result<SourcePlan, String> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(Path::new(&file.path))
        .map_err(|error| format!("Unsupported source file {}: {error}", file.path))?;
    let parsed = Parser::new(&allocator, &file.source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Failed to parse {}: {:?}",
            file.path, parsed.diagnostics
        ));
    }
    let semantic = SemanticBuilder::new_compiler().build(&parsed.program);
    if !semantic.diagnostics.is_empty() {
        return Err(format!(
            "Failed to analyze {}: {:?}",
            file.path, semantic.diagnostics
        ));
    }

    let mut imports = ImportCollector {
        file_path: &file.path,
        css_path,
        import_symbol: None,
        import_span: None,
    };
    imports.visit_program(&parsed.program);

    let mut collector = UsageCollector {
        source: &file.source,
        file_path: &file.path,
        is_module,
        scoping: semantic.semantic.scoping(),
        import_symbol: imports.import_symbol,
        candidates,
        edits: Vec::new(),
        emitted_candidates: BTreeSet::new(),
        module_refs: HashMap::new(),
        matched_module_refs: HashMap::new(),
        warnings: Vec::new(),
    };
    collector.visit_program(&parsed.program);

    if is_module
        && imports.import_symbol.is_some()
        && collector.module_refs.values().sum::<usize>()
            == collector.matched_module_refs.values().sum::<usize>()
        && !collector.module_refs.is_empty()
        && let Some(span) = imports.import_span
    {
        collector.edits.push(Edit {
            start: span.start as usize,
            end: consume_following_newline(&file.source, span.end as usize),
            replacement: String::new(),
        });
    }

    Ok(SourcePlan {
        edits: collector.edits,
        candidates: collector.emitted_candidates.into_iter().collect(),
        module_refs: collector.module_refs,
        matched_module_refs: collector.matched_module_refs,
        warnings: collector.warnings,
    })
}

struct ImportCollector<'s> {
    file_path: &'s str,
    css_path: &'s str,
    import_symbol: Option<SymbolId>,
    import_span: Option<Span>,
}

impl<'a> Visit<'a> for ImportCollector<'_> {
    fn visit_import_declaration(&mut self, declaration: &ImportDeclaration<'a>) {
        let resolved = resolve_import(self.file_path, declaration.source.value.as_str());
        if resolved == normalize_path(Path::new(self.css_path))
            && let Some(specifiers) = &declaration.specifiers
            && let Some(ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier)) =
                specifiers.first()
        {
            self.import_symbol = specifier.local.symbol_id.get();
            self.import_span = Some(declaration.span);
        }
        walk::walk_import_declaration(self, declaration);
    }
}

struct UsageCollector<'s> {
    source: &'s str,
    file_path: &'s str,
    is_module: bool,
    scoping: &'s Scoping,
    import_symbol: Option<SymbolId>,
    candidates: &'s HashMap<SelectorKey, Vec<String>>,
    edits: Vec<Edit>,
    emitted_candidates: BTreeSet<String>,
    module_refs: HashMap<String, usize>,
    matched_module_refs: HashMap<String, usize>,
    warnings: Vec<Warning>,
}

impl UsageCollector<'_> {
    fn module_member_name<'a>(&self, member: &'a StaticMemberExpression<'a>) -> Option<&'a str> {
        let Expression::Identifier(object) = &member.object else {
            return None;
        };
        let reference = object.reference_id.get()?;
        if self.scoping.get_reference(reference).symbol_id() != self.import_symbol {
            return None;
        }
        Some(member.property.name.as_str())
    }

    fn static_template(&self, template: &TemplateLiteral<'_>) -> Option<(String, Vec<String>)> {
        let mut value = String::new();
        let mut members = Vec::new();
        for (index, quasi) in template.quasis.iter().enumerate() {
            value.push_str(quasi.value.cooked.as_ref()?.as_str());
            let Some(expression) = template.expressions.get(index) else {
                continue;
            };
            let Expression::StaticMemberExpression(member) = expression else {
                return None;
            };
            let name = self.module_member_name(member)?.to_string();
            let candidates = self.candidates.get(&SelectorKey::Class(name.clone()))?;
            value.push_str(&candidates.join(" "));
            members.push(name);
        }
        Some((
            value.split_whitespace().collect::<Vec<_>>().join(" "),
            members,
        ))
    }

    fn global_element(&mut self, element: &JSXOpeningElement<'_>) {
        let mut id_candidates = Vec::new();
        let mut class_literal = None;

        for item in &element.attributes {
            let JSXAttributeItem::Attribute(attribute) = item else {
                continue;
            };
            let JSXAttributeName::Identifier(name) = &attribute.name else {
                continue;
            };
            let Some(JSXAttributeValue::StringLiteral(literal)) = &attribute.value else {
                continue;
            };
            if name.name == "id" {
                if let Some(candidates) = self
                    .candidates
                    .get(&SelectorKey::Id(literal.value.to_string()))
                {
                    id_candidates.extend(candidates.clone());
                }
            } else if name.name == "className" {
                class_literal = Some((literal.span, literal.value.to_string()));
            }
        }

        if let Some((span, value)) = class_literal {
            self.global_literal_edit(span, &value, &id_candidates);
        } else if !id_candidates.is_empty() {
            let end = element.span.end as usize;
            let mut insertion = if self.source[..end].ends_with("/>") {
                end - 2
            } else {
                end - 1
            };
            while insertion > element.span.start as usize
                && self.source.as_bytes()[insertion - 1].is_ascii_whitespace()
            {
                insertion -= 1;
            }
            for candidate in &id_candidates {
                self.emitted_candidates.insert(candidate.clone());
            }
            self.edits.push(Edit {
                start: insertion,
                end: insertion,
                replacement: format!(" className=\"{}\"", id_candidates.join(" ")),
            });
        }
    }

    fn global_literal_edit(&mut self, span: Span, value: &str, extra_candidates: &[String]) {
        let mut classes = value
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let original_classes = classes.clone();
        for class in original_classes {
            if let Some(candidates) = self.candidates.get(&SelectorKey::Class(class)) {
                for candidate in candidates {
                    self.emitted_candidates.insert(candidate.clone());
                    if !classes.contains(candidate) {
                        classes.push(candidate.clone());
                    }
                }
            }
        }
        for candidate in extra_candidates {
            self.emitted_candidates.insert(candidate.clone());
            if !classes.contains(candidate) {
                classes.push(candidate.clone());
            }
        }
        let replacement_value = classes.join(" ");
        if replacement_value == value {
            return;
        }
        let quote = self.source.as_bytes()[span.start as usize] as char;
        self.edits.push(Edit {
            start: span.start as usize,
            end: span.end as usize,
            replacement: format!("{quote}{replacement_value}{quote}"),
        });
    }
}

impl<'a> Visit<'a> for UsageCollector<'_> {
    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        if let Some(name) = self.module_member_name(member).map(str::to_string) {
            *self.module_refs.entry(name).or_default() += 1;
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_jsx_opening_element(&mut self, element: &JSXOpeningElement<'a>) {
        if !self.is_module {
            self.global_element(element);
            walk::walk_jsx_opening_element(self, element);
            return;
        }

        for item in &element.attributes {
            let JSXAttributeItem::Attribute(attribute) = item else {
                continue;
            };
            let JSXAttributeName::Identifier(name) = &attribute.name else {
                continue;
            };
            if name.name != "className" {
                continue;
            }
            let Some(JSXAttributeValue::ExpressionContainer(container)) = &attribute.value else {
                continue;
            };
            let (replacement_value, members) = match &container.expression {
                JSXExpression::StaticMemberExpression(member) => {
                    let Some(member_name) = self.module_member_name(member).map(str::to_string)
                    else {
                        continue;
                    };
                    let key = SelectorKey::Class(member_name.clone());
                    let Some(candidates) = self.candidates.get(&key) else {
                        continue;
                    };
                    (candidates.join(" "), vec![member_name])
                }
                JSXExpression::TemplateLiteral(template) => {
                    let Some(result) = self.static_template(template) else {
                        self.warnings.push(Warning {
                            code: "dynamic-class-name",
                            file: self.file_path.to_string(),
                            start: container.span.start as usize,
                            end: container.span.end as usize,
                            message: "The template contains a dynamic or unsupported class."
                                .to_string(),
                        });
                        continue;
                    };
                    result
                }
                _ => {
                    self.warnings.push(Warning {
                        code: "dynamic-class-name",
                        file: self.file_path.to_string(),
                        start: container.span.start as usize,
                        end: container.span.end as usize,
                        message: "Only static className values are supported.".to_string(),
                    });
                    continue;
                }
            };
            self.edits.push(Edit {
                start: container.span.start as usize,
                end: container.span.end as usize,
                replacement: serde_json::to_string(&replacement_value)
                    .expect("string serialization"),
            });
            for member in members {
                let key = SelectorKey::Class(member.clone());
                for candidate in &self.candidates[&key] {
                    self.emitted_candidates.insert(candidate.clone());
                }
                *self.matched_module_refs.entry(member).or_default() += 1;
            }
        }
        walk::walk_jsx_opening_element(self, element);
    }
}

fn resolve_import(file_path: &str, import: &str) -> PathBuf {
    let parent = Path::new(file_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    normalize_path(&parent.join(import))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn consume_following_newline(source: &str, end: usize) -> usize {
    if source[end..].starts_with("\r\n") {
        end + 2
    } else if source[end..].starts_with('\n') {
        end + 1
    } else {
        end
    }
}

fn apply_edits(source: &str, mut edits: Vec<Edit>) -> Result<String, String> {
    edits.sort_by_key(|edit| (edit.start, edit.end));
    for pair in edits.windows(2) {
        if pair[0].end > pair[1].start {
            return Err("Overlapping source edits were produced".to_string());
        }
    }
    let mut output = source.to_string();
    for edit in edits.into_iter().rev() {
        if edit.end > output.len() || edit.start > edit.end {
            return Err("Invalid source edit span".to_string());
        }
        output.replace_range(edit.start..edit.end, &edit.replacement);
    }
    Ok(output)
}

fn validate_js(path: &str, source: &str) -> Result<(), String> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(Path::new(path))
        .map_err(|error| format!("Unsupported source file {path}: {error}"))?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.diagnostics.is_empty() {
        Ok(())
    } else {
        Err(format!("Edited source no longer parses: {path}"))
    }
}

fn validate_css(source: &str) -> Result<(), String> {
    let allocator = oxc_css_parser::Allocator::default();
    CssParser::new(&allocator, source, Syntax::Css)
        .parse::<Stylesheet>()
        .map(|_| ())
        .map_err(|error| format!("Edited CSS no longer parses: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::plan_json;

    #[test]
    fn appends_a_global_class_and_retains_the_rule() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <div className='card' />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 1);
        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className='card p-[13px]' />;\n"
        );
        assert_eq!(response["warnings"][0]["code"], "retained-global-rule");
    }

    #[test]
    fn keeps_a_module_reference_when_a_sibling_rule_is_unsupported() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n.card::before { content: 'x'; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["files"], serde_json::json!([]));
        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
    }

    #[test]
    fn converts_an_exact_media_breakpoint() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": "@media (min-width: 48rem) { .card { padding: 13px; } }\n",
            "themeTokens": { "breakpoint-md": "48rem" },
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <div className=\"card\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["md:p-[13px]"]));
        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"card md:p-[13px]\" />;\n"
        );
    }

    #[test]
    fn converts_a_global_descendant_selector_to_an_arbitrary_variant() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": ".parent .child { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "export const Card = () => <span className=\"child\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["[.parent_&]:p-[13px]"])
        );
        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <span className=\"child [.parent_&]:p-[13px]\" />;\n"
        );
    }

    #[test]
    fn retains_a_css_module_class_referenced_by_composes() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n.featured { composes: card; color: red; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["convertedRules"], 0);
        assert_eq!(response["retainedRules"], 2);
        assert!(
            response["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "css-module-composes")
        );
    }

    #[test]
    fn normalizes_spacing_shorthand_before_mapping() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { margin: 1rem; margin-left: 2rem; }\n",
            "themeTokens": { "spacing": "0.25rem" },
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["mb-4", "ml-8", "mr-4", "mt-4"])
        );
    }

    #[test]
    fn prefers_an_exact_custom_theme_token() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "themeTokens": { "spacing-card": "13px" },
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["p-card"]));
    }

    #[test]
    fn converts_a_supported_pseudo_class_to_a_variant() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card:hover { color: red; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["candidates"],
            serde_json::json!(["hover:text-[red]"])
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn adds_a_class_name_for_a_global_id() {
        let request = serde_json::json!({
            "cssPath": "/project/global.css",
            "cssSource": "#hero { height: 100vh; }\n",
            "files": [{
                "path": "/project/Hero.tsx",
                "source": "export const Hero = () => <main id=\"hero\" />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["files"][0]["source"],
            "export const Hero = () => <main id=\"hero\" className=\"h-[100vh]\" />;\n"
        );
        assert_eq!(response["warnings"][0]["code"], "retained-global-rule");
    }

    #[test]
    fn migrates_a_static_css_module_template() {
        let request = serde_json::json!({
            "cssPath": "/project/Card.module.css",
            "cssSource": ".card { padding: 13px; }\n",
            "files": [{
                "path": "/project/Card.tsx",
                "source": "import styles from './Card.module.css';\nexport const Card = () => <div className={`${styles.card} featured`} />;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(
            response["files"][0]["source"],
            "export const Card = () => <div className=\"p-[13px] featured\" />;\n"
        );
        assert_eq!(response["convertedRules"], 1);
    }

    #[test]
    fn plans_a_direct_css_module_padding_migration() {
        let request = serde_json::json!({
            "cssPath": "/project/Button.module.css",
            "cssSource": ".button { padding: 13px; }\n",
            "files": [{
                "path": "/project/Button.tsx",
                "source": "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n"
            }]
        });

        let response: serde_json::Value =
            serde_json::from_str(&plan_json(&request.to_string()).unwrap()).unwrap();

        assert_eq!(response["candidates"], serde_json::json!(["p-[13px]"]));
        assert_eq!(response["convertedRules"], 1);
        assert_eq!(response["retainedRules"], 0);
        assert_eq!(
            response["files"][0]["source"],
            "export const Button = () => <button className=\"p-[13px]\">Save</button>;\n"
        );
        assert_eq!(response["files"][1]["source"], "\n");
    }
}
