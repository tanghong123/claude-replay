//! The kit (#s432, `design/embedding.md`, "Delivery"; in mdrev's shape, Hong's choice on #s514):
//! what each release publishes beside its binaries, so a host mounts the session view and the
//! session list from one release it pins and upgrades on purpose. [`files`] lays it out:
//!
//! - `package.json`: the release's version;
//! - `bundle/`: `index.js`, the kit's one entry (`kit/index.js`), and every module it imports,
//!   found by walking the imports from it, each exactly as this release's monitor serves it at
//!   `/monitor-ui/` ([`ui::asset`]), so `shared/` is a directory where the source tree has a link;
//!   the view's and the list's stylesheets; and `index.d.ts`, what the two take and answer;
//! - `docs/`: the guide (`kit/embedding-guide.md`, this release's facts written in) and the
//!   backend contract (`docs/backend-contract.md`);
//! - `conform.mjs`: the conformance check a host runs against its own server (#s434);
//! - `LICENSE`: the repository's, which the modules are copies under.
//!
//! `cargo run -p claude-monitor --example kit -- <dir>` writes it, and `scripts/kit.sh` packs it as
//! the release's `agent-monitor-kit-<version>.tar.gz` and its `.sha256`, which the Release workflow
//! publishes.

use std::collections::{BTreeMap, BTreeSet};

use crate::ui;

/// The release the kit is cut from: the workspace's version, which is the monitor's.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const INDEX: &str = include_str!("../kit/index.js");
const TYPES: &str = include_str!("../kit/index.d.ts");
const GUIDE: &str = include_str!("../kit/embedding-guide.md");
const CONFORM: &str = include_str!("../kit/conform.mjs");
const CONTRACT: &str = include_str!("../../docs/backend-contract.md");
const LICENSE: &str = include_str!("../../LICENSE");

/// The stylesheets the kit carries beside the modules: the view's and the list's.
const STYLESHEETS: [&str; 2] = ["session-view.css", "session-list.css"];

/// The kit's directory, and its archive's name less `.tar.gz`.
pub fn name() -> String {
    format!("agent-monitor-kit-{VERSION}")
}

/// What this release's monitor serves at `/monitor-ui/<name>`, or why there is nothing.
fn served(name: &str) -> Result<Vec<u8>, String> {
    ui::asset(&format!("monitor-ui/{name}"))
        .map(|response| response.body)
        .ok_or_else(|| {
            format!("the kit needs {name}, which the monitor does not serve at /monitor-ui/")
        })
}

/// The directory a module's own imports are relative to: "" or "shared/".
fn dir_of(name: &str) -> &str {
    name.rfind('/').map_or("", |at| &name[..=at])
}

/// `spec` from a module in `dir`, as a path under the bundle (`./x.js` from `shared/` is
/// `shared/x.js`), or an error for one that leaves the bundle.
fn resolve(dir: &str, spec: &str) -> Result<String, String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
    for part in spec.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts
                    .pop()
                    .ok_or_else(|| format!("{spec} from {dir:?} leaves the kit's bundle"))?;
            }
            name => parts.push(name),
        }
    }
    Ok(parts.join("/"))
}

/// Every module a source imports by a relative path: `import … from "./x.js"`, `export … from
/// "./x.js"`, `import "./x.js"` and `import("./x.js")`, each resolved from `dir`, in path order. It
/// reads the string right after each `from` or `import` word, so a quote elsewhere (an apostrophe
/// in a comment) cannot throw it off. A specifier that is not a relative path (an `import()` of a
/// server URL: mdrev's bundle, native mode's) is the server's to answer, not the kit's.
fn relative_imports(dir: &str, source: &str) -> Result<Vec<String>, String> {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '.';
    let mut found = BTreeSet::new();
    for keyword in ["from", "import"] {
        for (at, _) in source.match_indices(keyword) {
            if source[..at].chars().next_back().is_some_and(word) {
                continue;
            }
            let rest = source[at + keyword.len()..].trim_start();
            let rest = match keyword {
                "import" => rest.strip_prefix('(').map_or(rest, str::trim_start),
                _ => rest,
            };
            let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                continue;
            };
            let Some(end) = rest[1..].find(quote) else {
                continue;
            };
            let spec = &rest[1..1 + end];
            if spec.starts_with("./") || spec.starts_with("../") {
                found.insert(resolve(dir, spec)?);
            }
        }
    }
    Ok(found.into_iter().collect())
}

/// The modules the kit's entry imports and the ones they import, each named as the monitor serves
/// it under `/monitor-ui/` (`session-view.js`, `shared/time.js`), in path order.
pub fn modules() -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    let mut todo = relative_imports("", INDEX)?;
    while let Some(name) = todo.pop() {
        if seen.contains(&name) {
            continue;
        }
        let source = served(&name)?;
        let text = std::str::from_utf8(&source).map_err(|_| format!("{name} is not UTF-8"))?;
        todo.extend(relative_imports(dir_of(&name), text)?);
        seen.insert(name);
    }
    Ok(seen.into_iter().collect())
}

/// The guide with this release's facts written in: its version, the contract version, the mdrev
/// release the server pins and the Mermaid runtime's path.
fn guide() -> String {
    GUIDE
        .replace("{{version}}", VERSION)
        .replace("{{contract}}", &crate::routes::CONTRACT_VERSION.to_string())
        .replace("{{mdrev}}", mdrev::VERSION)
        .replace("{{mermaid}}", claude_replay_html::mermaid_runtime_path())
}

fn package_json() -> String {
    let package = serde_json::json!({
        "name": "agent-monitor-kit",
        "version": VERSION,
        "type": "module",
        "exports": "./bundle/index.js",
        "types": "./bundle/index.d.ts",
        "license": "MIT",
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&package).unwrap_or_default()
    )
}

/// Every file of the kit, as its path inside the kit's directory and its bytes, in path order.
pub fn files() -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = BTreeMap::new();
    out.insert("package.json".to_string(), package_json().into_bytes());
    out.insert("bundle/index.js".to_string(), INDEX.as_bytes().to_vec());
    out.insert("bundle/index.d.ts".to_string(), TYPES.as_bytes().to_vec());
    for name in modules()?.iter().map(String::as_str).chain(STYLESHEETS) {
        out.insert(format!("bundle/{name}"), served(name)?);
    }
    out.insert("docs/embedding-guide.md".to_string(), guide().into_bytes());
    out.insert(
        "docs/backend-contract.md".to_string(),
        CONTRACT.as_bytes().to_vec(),
    );
    out.insert("conform.mjs".to_string(), CONFORM.as_bytes().to_vec());
    out.insert("LICENSE".to_string(), LICENSE.as_bytes().to_vec());
    Ok(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kit() -> BTreeMap<String, Vec<u8>> {
        files().expect("the kit is laid out").into_iter().collect()
    }

    fn text(kit: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
        String::from_utf8(
            kit.get(path)
                .unwrap_or_else(|| panic!("the kit holds {path}"))
                .clone(),
        )
        .unwrap()
    }

    /// The names a component's `export const <constant> = Object.freeze([...])` lists.
    fn frozen_list(source: &str, constant: &str) -> Vec<String> {
        let start = source
            .find(&format!("export const {constant} = Object.freeze(["))
            .unwrap_or_else(|| panic!("{constant} is declared"));
        let list = &source[start..];
        let list = &list[list.find('[').unwrap() + 1..list.find("])").unwrap()];
        list.split(',')
            .map(|name| name.trim().trim_matches('"').to_string())
            .filter(|name| !name.is_empty())
            .collect()
    }

    /// The members an interface of `index.d.ts` declares, each with whether it is required.
    fn members(types: &str, interface: &str) -> Vec<(String, bool)> {
        let start = types
            .find(&format!("export interface {interface} {{"))
            .unwrap_or_else(|| panic!("index.d.ts declares {interface}"));
        let body = &types[start..];
        let body = &body[body.find('{').unwrap() + 1..body.find("\n}").unwrap()];
        body.lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("/**") && !line.starts_with('*') && !line.is_empty())
            .filter_map(|line| {
                let line = line.strip_prefix("readonly ").unwrap_or(line);
                let end = line.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
                let name = &line[..end];
                (!name.is_empty()).then(|| (name.to_string(), !line[end..].starts_with('?')))
            })
            .collect()
    }

    /// The bundle is the entry and its closure: every module the walk found, and nothing else but
    /// the two stylesheets and the declarations. The walk starts at `kit/index.js` and must reach
    /// both components, their backend and the list's store.
    #[test]
    fn the_bundle_is_the_entry_and_every_module_it_imports() {
        let modules = modules().unwrap();
        for entry in [
            "session-view.js",
            "session-list-view.js",
            "backend.js",
            "state.js",
        ] {
            assert!(
                modules.iter().any(|m| m == entry),
                "the walk reaches {entry}: {modules:?}"
            );
        }
        assert!(
            modules.iter().any(|m| m.starts_with("shared/")),
            "and the shared modules, as a directory of the bundle"
        );
        assert!(
            !modules.iter().any(|m| m == "app.js"),
            "the app is not in the kit: nothing the entry imports reaches it"
        );
        let kit = kit();
        let bundle: BTreeSet<&str> = kit
            .keys()
            .filter_map(|path| path.strip_prefix("bundle/"))
            .collect();
        let mut expected: BTreeSet<&str> = modules.iter().map(String::as_str).collect();
        expected.extend(["index.js", "index.d.ts"]);
        expected.extend(STYLESHEETS);
        assert_eq!(
            bundle, expected,
            "the bundle holds the closure and nothing more"
        );
        // Each module's own imports are in the bundle, at the path it names.
        for name in &modules {
            let source = text(&kit, &format!("bundle/{name}"));
            for dep in relative_imports(dir_of(name), &source).unwrap() {
                assert!(
                    bundle.contains(dep.as_str()),
                    "{name} imports {dep}, which the kit lacks"
                );
            }
        }
    }

    /// A bundled file is the source tree's own, byte for byte: `files` takes each from what the
    /// monitor serves (`ui::asset`, so the kit is what the app shell loads by construction), and the
    /// monitor serves the tree's files unchanged, `shared/` through the link to the html crate's
    /// modules. A served module that the server rewrote, or a link that pointed elsewhere, would
    /// put bytes in the kit that no node test has run.
    #[test]
    fn every_bundled_module_and_stylesheet_is_the_source_trees_file() {
        let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/codex-ui");
        let kit = kit();
        let mut checked = 0;
        for (path, bytes) in &kit {
            let Some(name) = path.strip_prefix("bundle/") else {
                continue;
            };
            if name.starts_with("index.") {
                continue;
            }
            let file = std::fs::read(tree.join(name))
                .unwrap_or_else(|why| panic!("{path} is {name} in the tree: {why}"));
            assert!(file == *bytes, "{path} is the tree's {name}, byte for byte");
            checked += 1;
        }
        assert!(checked > 40, "checked {checked} files");
    }

    /// The guide names this release's facts, with no placeholder left; the package names the version.
    #[test]
    fn the_guide_and_the_package_name_this_release() {
        let kit = kit();
        let guide = text(&kit, "docs/embedding-guide.md");
        assert!(!guide.contains("{{"), "every placeholder is written in");
        assert!(guide.contains(&format!("agent-monitor-kit-{VERSION}/")));
        assert!(guide.contains(&format!(
            "contract version {}",
            crate::routes::CONTRACT_VERSION
        )));
        assert!(guide.contains(&format!("pins mdrev {}", mdrev::VERSION)));
        assert!(guide.contains(&format!(
            r#"<meta name="mermaid-runtime" content="{}">"#,
            claude_replay_html::mermaid_runtime_path()
        )));
        let package: serde_json::Value = serde_json::from_str(&text(&kit, "package.json")).unwrap();
        assert_eq!(package["version"], VERSION);
        assert_eq!(package["type"], "module");
        assert_eq!(name(), format!("agent-monitor-kit-{VERSION}"));
    }

    /// The entry exports what the guide and the declarations name, from the modules that define them.
    #[test]
    fn the_entry_exports_what_the_declarations_declare() {
        let kit = kit();
        let types = text(&kit, "bundle/index.d.ts");
        for name in [
            "mountSessionView",
            "mountSessionList",
            "backendHandle",
            "useBackend",
            "persist",
            "mergedBackend",
            "splitMachineId",
        ] {
            assert!(
                INDEX.contains(&format!("export {{ {name}"))
                    || INDEX.contains(&format!(", {name} }}")),
                "index.js exports {name}"
            );
            assert!(
                types.contains(&format!("export function {name}(")),
                "index.d.ts declares {name}"
            );
        }
    }

    /// The options each component takes that the kit does not offer a host: the ones
    /// agent-monitor's own page and the native app use beside the ruled ones (#s436, #s476). The
    /// view's frame is the native app's (#s436: "For native app is fine, this one is controlled by
    /// me"); its back-to-the-list, parent and pane-layout controls are the app's own chrome; the
    /// list's projects are native mode's, and its placing of its parts in a host's bar and layers
    /// is the app's. `keyboard` is the app's too: its one choice besides the default, `"page"`,
    /// hands every key to the host's own keymap, where #s436 offered keys answered only while the
    /// reader works in the part, which is the default. Offering one is a contract change for the
    /// owner to rule on first.
    const VIEW_WITHHELD: [&str; 7] = [
        "keyboard",
        "frame",
        "parentShown",
        "openList",
        "openSidebar",
        "paneLayout",
        "paneCount",
    ];
    const LIST_WITHHELD: [&str; 6] = [
        "keyboard",
        "projectTree",
        "projectActions",
        "newSession",
        "layer",
        "place",
    ];

    /// The declarations follow each component's own lists: every option it takes but the ones the
    /// kit withholds, each optional control, and the view's required options as its doc comment
    /// names them, required and nothing else. A new option fails here until it is declared or
    /// withheld.
    #[test]
    fn the_declarations_follow_each_components_options() {
        let view = include_str!("codex-ui/session-view.js");
        let list = include_str!("codex-ui/session-list-view.js");
        for (source, options, controls, withheld) in [
            (
                view,
                "SessionViewOptions",
                "SessionViewControls",
                &VIEW_WITHHELD[..],
            ),
            (
                list,
                "SessionListOptions",
                "SessionListControls",
                &LIST_WITHHELD[..],
            ),
        ] {
            let declared = members(TYPES, options);
            let names: BTreeSet<&str> = declared.iter().map(|(n, _)| n.as_str()).collect();
            let all: BTreeSet<String> = frozen_list(source, "MOUNT_OPTIONS").into_iter().collect();
            for name in withheld {
                assert!(
                    all.contains(*name),
                    "{name} is an option the component takes"
                );
            }
            let taken: BTreeSet<&str> = all
                .iter()
                .map(String::as_str)
                .filter(|name| !withheld.contains(name))
                .collect();
            assert_eq!(
                names, taken,
                "{options} declares the options the component takes, less the withheld ones"
            );
            let guide = GUIDE;
            for name in withheld {
                assert!(
                    !guide.contains(&format!("`{name}`")),
                    "the guide offers no withheld option: {name}"
                );
            }
            let shown: BTreeSet<String> = members(TYPES, controls)
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            let optional: BTreeSet<String> = frozen_list(source, "OPTIONAL_CONTROLS")
                .into_iter()
                .collect();
            assert_eq!(shown, optional, "{controls} declares the optional controls");
        }
        let doc = &view[view.find("Required: ").unwrap()..];
        let doc = &doc[..doc.find(". The rest").unwrap()];
        let required: BTreeSet<&str> = doc.split('`').skip(1).step_by(2).collect();
        let declared: BTreeSet<String> = members(TYPES, "SessionViewOptions")
            .into_iter()
            .filter(|(_, required)| *required)
            .map(|(n, _)| n)
            .collect();
        let declared: BTreeSet<&str> = declared.iter().map(String::as_str).collect();
        assert_eq!(
            declared, required,
            "the view's required options, and only those, are required"
        );
        assert!(
            members(TYPES, "SessionListOptions")
                .iter()
                .all(|(_, required)| !required),
            "every option of the list is optional"
        );
    }

    /// A check of the walk that does not share its parser: every string in a bundled module that
    /// reads as a relative path, in any quotes and wherever it stands (an import of a form the walk
    /// does not read, a `new URL("./x.js", …)`), names a file of the bundle; none is a template
    /// literal, whose path the kit cannot know; and no module asks `import.meta`, whose answer is
    /// the host's path, not ours.
    #[test]
    fn every_relative_path_a_bundled_module_names_is_in_the_bundle() {
        let kit = kit();
        let mut named = 0;
        for name in modules().unwrap() {
            let source = text(&kit, &format!("bundle/{name}"));
            assert!(
                !source.contains("import.meta"),
                "{name} reads import.meta, which names the host's path"
            );
            for quote in ['"', '\'', '`'] {
                for lead in ["./", "../"] {
                    let open = format!("{quote}{lead}");
                    for (at, _) in source.match_indices(&open) {
                        let rest = &source[at + 1..];
                        let spec = &rest[..rest.find(quote).unwrap_or(rest.len())];
                        assert!(
                            quote != '`',
                            "{name} builds a relative path in a template literal: {spec}"
                        );
                        let path = resolve(dir_of(&name), spec).unwrap();
                        assert!(
                            kit.contains_key(&format!("bundle/{path}")),
                            "{name} names {spec}, which is not in the kit's bundle"
                        );
                        named += 1;
                    }
                }
            }
        }
        assert!(named > 40, "the modules name {named} relative paths");
    }

    /// The backend handle's declared options are options `backendHandle` takes, by name.
    #[test]
    fn the_handles_declared_options_are_its_own() {
        let backend = include_str!("codex-ui/backend.js");
        let start = backend.find("export function backendHandle({").unwrap();
        let params = &backend[start..];
        let params = &params[params.find('{').unwrap() + 1..params.find('}').unwrap()];
        let taken: BTreeSet<&str> = params
            .split(',')
            .map(|param| param.split('=').next().unwrap().trim())
            .collect();
        let declared = members(TYPES, "BackendHandleOptions");
        assert!(!declared.is_empty());
        for (name, required) in &declared {
            assert!(taken.contains(name.as_str()), "backendHandle takes {name}");
            assert!(!required, "{name} is optional");
        }
    }

    #[test]
    fn imports_are_read_from_every_form_and_resolved_from_the_module() {
        let source = "import { a } from \"./a.js\";\nexport { b } from './shared/b.js';\nimport \"./c.js\";\nconst d = await import(\"./d.js\");\nconst e = await import(backend().url(\"/x.js\"));\n// a string \"./not.js\" in prose\n";
        assert_eq!(
            relative_imports("", source).unwrap(),
            vec!["a.js", "c.js", "d.js", "shared/b.js"]
        );
        assert_eq!(
            relative_imports("shared/", "import { x } from \"./x.js\";").unwrap(),
            vec!["shared/x.js"]
        );
        assert_eq!(
            relative_imports("shared/", "import { x } from \"../y.js\";").unwrap(),
            vec!["y.js"]
        );
        assert!(
            relative_imports("", "import { x } from \"../y.js\";").is_err(),
            "nothing outside the bundle"
        );
    }
}
