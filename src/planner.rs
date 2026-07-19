use std::{
    collections::{BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Expression, ImportDeclaration, ImportDeclarationSpecifier, JSXAttributeItem, JSXAttributeName,
    JSXAttributeValue, JSXExpression, JSXOpeningElement, StaticMemberExpression,
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
    let rules = parse_css_rules(&request.css_path, &request.css_source)?;
    let is_module = request.css_path.ends_with(".module.css");

    let mut candidate_map: HashMap<SelectorKey, Vec<String>> = HashMap::new();
    for rule in &rules {
        if let Some(key) = &rule.key
            && rule.warning.is_none()
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

fn parse_css_rules(path: &str, source: &str) -> Result<Vec<RulePlan>, String> {
    let allocator = oxc_css_parser::Allocator::default();
    let mut parser = CssParser::new(&allocator, source, Syntax::Css);
    let stylesheet = parser
        .parse::<Stylesheet>()
        .map_err(|error| format!("Failed to parse {path}: {error:?}"))?;

    let mut rules = Vec::new();
    for statement in &stylesheet.statements {
        let Statement::QualifiedRule(rule) = statement else {
            continue;
        };

        let selector = source[rule.selector.span.start..rule.selector.span.end].to_string();
        let key = simple_selector(rule);
        let mut candidates = Vec::new();
        let mut warning = key.is_none().then_some("unsupported-selector");

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
            let value = source[declaration.colon_span.end as usize..declaration.span.end]
                .trim()
                .trim_end_matches(';')
                .trim();
            match declaration_to_candidate(property, value) {
                Some(candidate) => candidates.push(candidate),
                None => warning = Some("unsupported-declaration"),
            }
        }

        if candidates.is_empty() {
            warning = Some("unsupported-declaration");
        }
        candidates.sort();
        candidates.dedup();
        rules.push(RulePlan {
            span: rule.span.start..rule.span.end,
            selector,
            key,
            candidates,
            warning,
        });
    }
    Ok(rules)
}

fn simple_selector(rule: &oxc_css_parser::ast::QualifiedRule<'_>) -> Option<SelectorKey> {
    let selector = rule.selector.selectors.first()?;
    if selector.children.len() != 1 {
        return None;
    }
    let ComplexSelectorChild::CompoundSelector(compound) = &selector.children[0] else {
        return None;
    };
    if compound.children.len() != 1 {
        return None;
    }
    match &compound.children[0] {
        SimpleSelector::Class(class) => {
            literal_ident(&class.name).map(|name| SelectorKey::Class(name.to_string()))
        }
        SimpleSelector::Id(id) => {
            literal_ident(&id.name).map(|name| SelectorKey::Id(name.to_string()))
        }
        _ => None,
    }
}

fn literal_ident<'a>(ident: &'a InterpolableIdent<'a>) -> Option<&'a str> {
    match ident {
        InterpolableIdent::Literal(ident) => Some(ident.name),
        _ => None,
    }
}

fn declaration_to_candidate(property: &str, value: &str) -> Option<String> {
    if value.is_empty() || value.contains(['[', ']', ';']) {
        return None;
    }
    let exact = match (property, value) {
        ("padding", "1rem") => Some("p-4"),
        ("margin", "1rem") => Some("m-4"),
        ("display", "flex") => Some("flex"),
        ("display", "grid") => Some("grid"),
        ("display", "none") => Some("hidden"),
        _ => None,
    };
    if let Some(candidate) = exact {
        return Some(candidate.to_string());
    }

    let prefix = match property {
        "padding" => "p",
        "margin" => "m",
        "gap" => "gap",
        "width" => "w",
        "height" => "h",
        "color" => "text",
        "background-color" => "bg",
        "border-radius" => "rounded",
        "font-size" => "text",
        _ => return Some(format!("[{property}:{}]", arbitrary_value(value))),
    };
    Some(format!("{prefix}-[{}]", arbitrary_value(value)))
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
        file_path: &file.path,
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
    file_path: &'s str,
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
}

impl<'a> Visit<'a> for UsageCollector<'_> {
    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        if let Some(name) = self.module_member_name(member).map(str::to_string) {
            *self.module_refs.entry(name).or_default() += 1;
        }
        walk::walk_static_member_expression(self, member);
    }

    fn visit_jsx_opening_element(&mut self, element: &JSXOpeningElement<'a>) {
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
            let JSXExpression::StaticMemberExpression(member) = &container.expression else {
                self.warnings.push(Warning {
                    code: "dynamic-class-name",
                    file: self.file_path.to_string(),
                    start: container.span.start as usize,
                    end: container.span.end as usize,
                    message: "Only direct CSS Module members are supported.".to_string(),
                });
                continue;
            };
            let Some(member_name) = self.module_member_name(member).map(str::to_string) else {
                continue;
            };
            let key = SelectorKey::Class(member_name.clone());
            let Some(candidates) = self.candidates.get(&key) else {
                continue;
            };
            let replacement =
                serde_json::to_string(&candidates.join(" ")).expect("string serialization");
            self.edits.push(Edit {
                start: container.span.start as usize,
                end: container.span.end as usize,
                replacement,
            });
            for candidate in candidates {
                self.emitted_candidates.insert(candidate.clone());
            }
            *self.matched_module_refs.entry(member_name).or_default() += 1;
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
