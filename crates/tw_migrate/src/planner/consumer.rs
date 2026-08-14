use super::*;

pub(super) fn plan_consumer_file(
    file: &SourceFile,
    css_path: &str,
    is_module: bool,
    candidates: &HashMap<SelectorKey, Vec<String>>,
    preserved_module_classes: &BTreeSet<String>,
    module_rule_classes: Option<&BTreeSet<String>>,
    utility_prefix: Option<&str>,
    vue_unscoped: bool,
    vue_module: bool,
) -> Result<SourcePlan, String> {
    // Vue scoped styles never apply outside their own SFC, and a `.vue` file
    // is not parseable JS: the only live pairing is an SFC consuming its own
    // scoped blocks through the HTML contract. A `.vue` consumer of any other
    // stylesheet is an opaque reference that can only retain a module.
    let stylesheet_is_vue = is_vue_path(css_path);
    let file_is_vue = is_vue_path(&file.path);
    if stylesheet_is_vue && vue_module {
        if file_is_vue && file.has_analyzable_context(css_path) {
            return Ok(plan_vue_module_file(
                file,
                css_path,
                candidates,
                module_rule_classes,
                utility_prefix,
            ));
        }
        return Ok(SourcePlan::default());
    }
    if stylesheet_is_vue && vue_unscoped {
        if file_is_vue
            || Path::new(&file.path)
                .extension()
                .is_some_and(|ext| ext == "html")
        {
            return Ok(plan_html_file(file, css_path, candidates, utility_prefix));
        }
        return plan_batch_source_file(file, css_path, false, candidates, preserved_module_classes);
    }
    if stylesheet_is_vue || file_is_vue {
        if file_is_vue && file.has_analyzable_context(css_path) {
            return Ok(plan_html_file(file, css_path, candidates, utility_prefix));
        }
        if file_is_vue && !stylesheet_is_vue {
            return Ok(opaque_reference_plan(file, css_path, is_module));
        }
        return Ok(SourcePlan::default());
    }
    if Path::new(&file.path)
        .extension()
        .is_some_and(|extension| extension == "html")
    {
        return Ok(plan_html_file(file, css_path, candidates, utility_prefix));
    }
    plan_batch_source_file(
        file,
        css_path,
        is_module,
        candidates,
        preserved_module_classes,
    )
}

pub(crate) fn is_recoverable_input_error(error: &str) -> bool {
    (!error.starts_with("Failed to parse edited CSS") && error.starts_with("Failed to parse "))
        || error.starts_with("Failed to analyze ")
        || error.starts_with("Unsupported source file ")
}
