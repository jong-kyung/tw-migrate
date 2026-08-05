# RFC: Dynamic Class Expression Migration

## Status

Proposed

## Summary

`tw-migrate` currently rewrites static React `className` values, static Vue `class` and `:class` values, CSS Module members, and static HTML class attributes. It retains logical expressions, conditional expressions, and other runtime class construction because the planner cannot identify which expression leaves produce DOM classes.

This RFC adds a bounded expression grammar for React and Vue. The planner rewrites class-producing string literals and CSS Module members in the result positions of logical and conditional expressions without evaluating their runtime conditions. It preserves unsupported sibling leaves and reports them through the existing dynamic-class diagnostics.

The RFC also defines a later cleanup phase for removing fully converted non-module class tokens from React, Vue, and HTML consumers. A token remains when a retained rule, selector relationship, or package-local stylesheet still needs it. Global CSS rules remain in their source files.

Delivery is split into three independently reviewed changes:

1. React and Next.js logical and conditional expressions.
2. Vue logical and conditional `:class` expressions.
3. Proven removal of converted non-module class tokens across React, Vue, and HTML.

## Goals

1. Convert class leaves in React and Next.js `className` logical and conditional expressions.
2. Convert the same expression forms in Vue 3 `:class` bindings.
3. Support both non-module string classes and direct CSS Module members.
4. Preserve runtime conditions and surrounding source bytes through leaf-span edits.
5. Permit partial conversion when supported and unsupported result branches coexist.
6. Treat mutually exclusive conditional branches as separate class coexistence sets.
7. Remove fully converted non-module class tokens when package-scoped CSS analysis proves that no retained styling or selector condition needs them.
8. Apply non-module token removal consistently to React, Vue scoped and unscoped styles, and static HTML.
9. Preserve existing candidate validation, conflict handling, transactional writes, determinism, and second-run idempotency.

## Non-Goals

1. Evaluating conditions or resolving runtime variable values.
2. Supporting arrays, object class maps, concatenation, or interpolated template literals.
3. Supporting `clsx`, `classnames`, `cn`, or arbitrary helper calls.
4. Following aliases such as `const activeClass = styles.active`.
5. Supporting computed CSS Module access such as `styles[name]` or `$style[name]`.
6. Supporting named Vue CSS Modules such as `<style module="classes">`.
7. Rewriting strings passed to arbitrary functions merely because the call appears inside `className` or `:class`.
8. Removing global CSS rules automatically.
9. Removing `id` attributes after ID selector conversion.
10. Proving runtime DOM API compatibility for class tokens used by `querySelector`, tests, analytics, or third-party scripts.
11. Scanning unsupported template formats or dependency-owned runtime markup.

## Terminology

- **Class expression**: the JavaScript expression assigned to React `className` or Vue `:class`.
- **Result position**: an expression location whose value may become the class binding result under this RFC's bounded grammar.
- **Class leaf**: a supported string literal or direct CSS Module member in a result position.
- **Opaque position**: a condition or unsupported expression that the planner does not interpret as a class value.
- **Coexistence set**: class leaves that can apply to one element at the same time.
- **Non-module token**: a literal DOM class name rather than a CSS Module member.
- **Selector anchor**: a class that a generated or retained selector condition still needs on another element, such as `parent` in `.parent .child`.
- **Shadow stylesheet**: another package-local stylesheet that can apply rules to the same non-module class token.

## Supported Expression Grammar

### React and Next.js

The planner continues to support direct static forms and adds the following result grammar:

```tsx
<div className={isActive && "active"} />
<div className={preferred || "fallback"} />
<div className={preferred ?? styles.fallback} />
<div className={isActive ? styles.active : styles.inactive} />
<div className={isActive ? "active" : null} />
```

The planner traverses:

- the right operand of `&&`, `||`, and `??`;
- the consequent and alternate of a conditional expression;
- nested logical and conditional expressions reached through those result positions; and
- parentheses and TypeScript `as`, `satisfies`, and non-null wrappers around a supported result.

The planner does not traverse a logical expression's left operand or a conditional expression's test as a class-producing position:

```tsx
<div className={styles.enabled ? "active" : "inactive"} />
```

In this example, `styles.enabled` remains an opaque condition. It does not count as a class application and cannot authorize removal of its CSS Module export.

Supported leaves are:

- string literals containing zero or more whitespace-separated class tokens;
- direct members of the selected CSS Module binding, such as `styles.active`; and
- `null`, `undefined`, `false`, and empty strings as warning-free no-op leaves.

Other result leaves remain unchanged and produce the existing `dynamic-class-name` warning. A supported sibling branch may still migrate.

### Vue 3

Vue `:class` uses the same result grammar:

```vue
<div :class="isActive && 'active'" />
<div :class="preferred || 'fallback'" />
<div :class="isActive ? $style.active : $style.inactive" />
<div :class="isActive ? 'active' : null" />
```

The Vue parser lowers each supported leaf to an authored-file byte span. The native planner applies the same candidate and conflict rules used for React.

Only the default `$style.name` binding is supported. Named module bindings remain covered by `unsupported-sfc-block`. An opaque `$style` use keeps the module live under the existing module-closure rules.

An unsupported result leaf produces `dynamic-template-class`. For scoped and unscoped Vue rules, the unsupported leaf also keeps any class reachability that it could represent opaque. Supported sibling leaves may receive candidates, but a rule or token remains when the opaque branch prevents safe cleanup.

## Rewrite Semantics

### Preserve expression behavior

The planner edits class leaves rather than rebuilding the containing expression. Conditions, operator choice, parentheses, and unrelated bytes remain unchanged.

A generated utility string is non-empty whenever it replaces a non-empty class leaf, so replacing a supported leaf preserves its truthiness for `&&`, `||`, and `??` evaluation.

For example:

```tsx
// Before
<div className={isActive && "active"} />

// After `.active { color: red; }`
<div className={isActive && "text-[red]"} />
```

```vue
<!-- Before -->
<div :class="isActive && 'active'" />

<!-- After `.active { color: red; }` -->
<div :class="isActive && 'text-[red]'" />
```

### Partial conversion

The planner handles each supported result leaf independently:

```tsx
// Before
<div className={isActive ? styles.active : getClass()} />

// After
<div className={isActive ? "text-[red]" : getClass()} />
```

The `getClass()` branch remains unchanged and receives `dynamic-class-name`. Its presence does not undo a safe edit to `styles.active`, but any unresolved CSS Module reference still blocks rule, import, or module deletion through the existing reference accounting.

### Conditional branch conflicts

The consequent and alternate of one conditional expression are mutually exclusive coexistence sets. Utilities in opposite branches may affect the same Tailwind property without triggering `module-utilities-conflict` or `batch-stylesheet-conflict` against each other.

A conditional leaf still conflicts with classes that can coexist with it. Existing Tailwind utilities in the same leaf, static classes on the same element, and candidates outside the mutually exclusive branch keep the existing conflict behavior.

### CSS Module leaves

A direct CSS Module leaf is replaced with generated utilities only when its module rules can be removed safely. If a converted candidate must coexist with a retained module rule, the planner keeps the module member and adds the candidate inside the same conditional branch. It must not hoist a conditional candidate to an unconditional static class attribute.

When all module references become removable, existing rule, block, import, and file cleanup behavior applies.

## Non-Module Class Token Removal

The first two delivery phases retain existing non-module tokens and append generated utilities. The third phase changes both static and newly supported dynamic consumers to remove a non-module token when CSS semantics prove it removable.

The policy applies to:

- React and Next.js static and supported dynamic `className` leaves;
- Vue literal `class` attributes and supported static or dynamic `:class` leaves, including classes owned by scoped blocks;
- static HTML `class` attributes.

A non-module token is removable at a consumer only when all of the following hold:

1. Every package-local rule that targets the token at that consumer is representable, validated, and selected for conversion.
2. No retained declaration, rule, at-rule, source-map ambiguity, cascade conflict, or opaque class surface still needs the token.
3. No generated arbitrary variant or retained selector uses the token as an ancestor, sibling, or other selector anchor.
4. The package stylesheet shadow corpus contains no other rule that can apply through the token without an equivalent candidate at that consumer.
5. Every candidate that replaces the token remains in the same runtime branch and stylesheet context as the original token.

If any condition fails, the planner keeps the token and follows the existing retain-with-append behavior.

### Selector anchors

A converted selector can still require an original token:

```css
.active .child {
  color: red;
}
```

A generated child utility such as `[.active_&]:text-[red]` still depends on the ancestor's `active` token. The planner therefore marks `active` as an anchor and does not remove it.

A target token used only to locate its own converted rule does not remain solely for that purpose:

```css
.active:hover {
  color: red;
}
```

After conversion to `hover:text-[red]`, the target element no longer needs `active` unless another rule or anchor condition requires it.

### Shadow stylesheet boundary

The removal proof uses the package-local stylesheet corpus already discovered for migration and safety analysis, including non-selected stylesheets that can shadow a selected target. A positional single-stylesheet migration must retain a token when another discovered stylesheet can target it.

Stylesheets and runtime markup outside the supported package analysis boundary remain out of scope. This matches the existing closed-world boundaries for selector and Vue shadow analysis.

### Global CSS and IDs

Global rules remain in their source files and continue to emit `retained-global-rule`. Removing a proven consumer token does not authorize global rule deletion.

The planner never removes an `id` attribute. IDs can participate in labels, ARIA relationships, fragments, DOM APIs, and external contracts that class cleanup does not analyze.

## Architecture

### Shared expression leaf model

React and Vue produce the same internal leaf information:

- authored file path and byte span;
- selector key;
- module or non-module ownership;
- branch/coexistence identity;
- surrounding consumer identity;
- whether the leaf is writable;
- any opaque sibling or condition references relevant to cleanup.

The Rust planner remains responsible for candidate selection, conflicts, reference accounting, and final edits. The Vue TypeScript layer only lowers compiler locations and expression metadata into authored-file spans.

### Branch-aware matches

Candidate matches need branch identity in addition to element and source spans. Conflict checks compare candidates only when their coexistence sets overlap. Consequent and alternate branches do not overlap; a conditional result and an unconditional class on the same element do.

### Package class retention index

The third phase derives one retention index from parsed package stylesheets. For each non-module class it records:

- target rules and their conversion state;
- retained rules and warnings;
- selector-anchor use;
- stylesheet contexts that can reach each consumer;
- shadow selectors from non-selected package stylesheets.

React, Vue, and HTML rewrites consult this shared result instead of implementing separate removal policies.

## Diagnostics

The feature reuses existing warning codes:

- `dynamic-class-name` for unsupported React result leaves;
- `dynamic-template-class` for unsupported Vue result leaves;
- existing CSS Module reference warnings for opaque module access;
- existing conflict and retained-rule warnings when a class leaf or token cannot be removed.

Supported logical and conditional expressions do not emit a dynamic warning. `null`, `undefined`, `false`, and empty-string leaves do not emit warnings.

Warnings use the smallest authored span that identifies the unsupported leaf when the parser provides one. Ordering remains deterministic.

## Delivery

### Phase 1: React and Next.js expressions

- Extend JSX expression collection with the bounded result grammar.
- Rewrite global string leaves and direct CSS Module members.
- Add branch identities so opposite conditional branches do not conflict.
- Keep non-module token append behavior until Phase 3.
- Preserve existing module cleanup and unsupported-reference accounting.

### Phase 2: Vue expressions

- Parse the same result grammar for `:class` with the target project's Vue compiler and the existing OXC expression parser.
- Lower leaf spans to authored `.vue` offsets.
- Support non-module strings and default `$style.name` leaves.
- Keep named modules and unsupported expressions opaque.
- Keep non-module token append behavior until Phase 3.

### Phase 3: Non-module token removal

- Build the shared package class retention index.
- Remove proven static and dynamic non-module tokens in React, Vue, and HTML.
- Retain tokens needed by rules, selector anchors, shadow stylesheets, or opaque class surfaces.
- Keep global rules and IDs unchanged.

Each phase merges separately. Public documentation describes only implemented phases.

## Testing Strategy

### Rust planner tests

- `&&`, `||`, and `??` rewrite only their right result operand.
- Conditional tests remain opaque while both result branches migrate.
- Nested expressions and transparent TypeScript wrappers migrate.
- `null`, `undefined`, `false`, and empty strings are warning-free.
- Supported and unsupported result branches permit partial conversion.
- Opposite conditional branches may contain overlapping utilities.
- Conditional candidates still conflict with classes that can coexist with them.
- CSS Module imports disappear only after every reference is removable.
- Computed, aliased, helper-call, array, object, and interpolated-template forms retain.

### Vue and Node tests

- Vue compiler locations lower to exact authored leaf spans.
- Dynamic global and `$style` leaves rewrite without changing surrounding directive bytes.
- Opaque conditions and unsupported sibling leaves keep closure accounting conservative.
- Named modules remain unsupported.
- Edited SFCs reparse with the target project's compiler.

### Non-module removal tests

- Fully converted static and dynamic tokens disappear from React and Vue.
- Fully converted static tokens disappear from HTML.
- Vue scoped tokens follow the same removal policy.
- Retained rules keep their tokens.
- Ancestor and sibling selector anchors keep their tokens.
- A matching non-selected package stylesheet keeps the token.
- IDs remain unchanged.
- Positional single-file migration uses the package shadow corpus.

### Packaged and browser tests

- Packaged snapshots cover final source bytes, warnings, changed files, and second-run idempotency for each runtime.
- Controlled browser cases render both sides of conditional branches and compare computed styles before and after migration.
- Existing static migration snapshots update only in Phase 3, when the token policy changes.

## Success Criteria

1. Supported React and Vue expressions migrate without dynamic-class warnings.
2. The planner changes only class-producing result leaves and preserves conditions byte-for-byte.
3. Partial conversion never removes a rule, module binding, or token still needed by an unsupported branch.
4. Mutually exclusive branches do not block each other solely because their utilities overlap.
5. CSS Module cleanup remains conservative and complete when every reference migrates.
6. Non-module tokens disappear only after rule, anchor, and package stylesheet shadow checks pass.
7. Global CSS rules and IDs remain unchanged.
8. React, Vue, and HTML use one non-module removal policy.
9. Every edited source reparses, controlled browser styles match, and a second run produces no diff.

## Accepted Trade-offs

1. Logical-expression traversal treats only the right operand as a class result. This covers the requested trailing-class patterns without interpreting the left operand's runtime role.
2. Runtime users of DOM class names, including selectors in application scripts and external tools, do not block non-module token removal. The removal proof covers stylesheet semantics within the supported package boundary.
3. Package-local shadow analysis can retain tokens for selectors whose full ancestry could never match. Conservative false retention is preferable to removing a live styling hook.
4. Global CSS files can contain dead rules after proven consumer tokens disappear because automatic global rule deletion remains outside the migration contract.
5. React and Vue use different parsers, but both lower into one planner leaf model rather than duplicating candidate and cleanup rules.

## Deferred Work

- Array and object class binding forms.
- Known class helper calls and configurable helper names.
- Interpolated templates and string concatenation.
- Alias and computed-member analysis.
- Named Vue CSS Modules.
- Runtime DOM class-hook analysis.
- Automatic global CSS rule deletion.
