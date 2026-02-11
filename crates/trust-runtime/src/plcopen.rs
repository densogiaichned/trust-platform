//! PLCopen XML interchange (strict subset for ST projects).

#![allow(missing_docs)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use trust_syntax::lexer::{lex, TokenKind};
use trust_syntax::parser;
use trust_syntax::syntax::{SyntaxKind, SyntaxNode};

const PLCOPEN_NAMESPACE: &str = "http://www.plcopen.org/xml/tc6_0200";
const PROFILE_NAME: &str = "trust-st-strict-v1";
const SOURCE_MAP_DATA_NAME: &str = "trust.sourceMap";
const VENDOR_EXT_DATA_NAME: &str = "trust.vendorExtensions";
const VENDOR_EXTENSION_HOOK_FILE: &str = "plcopen.vendor-extensions.xml";
const IMPORTED_VENDOR_EXTENSION_FILE: &str = "plcopen.vendor-extensions.imported.xml";
const MIGRATION_REPORT_FILE: &str = "interop/plcopen-migration-report.json";

const SIEMENS_LIBRARY_SHIMS: &[VendorLibraryShim] = &[
    VendorLibraryShim {
        source_symbol: "SFB3",
        replacement_symbol: "TP",
        notes: "Siemens pulse timer alias mapped to IEC TP.",
    },
    VendorLibraryShim {
        source_symbol: "SFB4",
        replacement_symbol: "TON",
        notes: "Siemens on-delay timer alias mapped to IEC TON.",
    },
    VendorLibraryShim {
        source_symbol: "SFB5",
        replacement_symbol: "TOF",
        notes: "Siemens off-delay timer alias mapped to IEC TOF.",
    },
];

const ROCKWELL_LIBRARY_SHIMS: &[VendorLibraryShim] = &[VendorLibraryShim {
    source_symbol: "TONR",
    replacement_symbol: "TON",
    notes:
        "Rockwell retentive timer alias mapped to IEC TON (review retentive semantics manually).",
}];

const SCHNEIDER_LIBRARY_SHIMS: &[VendorLibraryShim] = &[
    VendorLibraryShim {
        source_symbol: "R_EDGE",
        replacement_symbol: "R_TRIG",
        notes: "Schneider/CODESYS edge alias mapped to IEC R_TRIG.",
    },
    VendorLibraryShim {
        source_symbol: "F_EDGE",
        replacement_symbol: "F_TRIG",
        notes: "Schneider/CODESYS edge alias mapped to IEC F_TRIG.",
    },
];

const MITSUBISHI_LIBRARY_SHIMS: &[VendorLibraryShim] = &[
    VendorLibraryShim {
        source_symbol: "DIFU",
        replacement_symbol: "R_TRIG",
        notes: "Mitsubishi differential-up alias mapped to IEC R_TRIG.",
    },
    VendorLibraryShim {
        source_symbol: "DIFD",
        replacement_symbol: "F_TRIG",
        notes: "Mitsubishi differential-down alias mapped to IEC F_TRIG.",
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct PlcopenProfile {
    pub namespace: &'static str,
    pub profile: &'static str,
    pub version: &'static str,
    pub strict_subset: Vec<&'static str>,
    pub unsupported_nodes: Vec<&'static str>,
    pub compatibility_matrix: Vec<PlcopenCompatibilityMatrixEntry>,
    pub source_mapping: &'static str,
    pub vendor_extension_hook: &'static str,
    pub round_trip_limits: Vec<&'static str>,
    pub known_gaps: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlcopenExportReport {
    pub output_path: PathBuf,
    pub source_map_path: PathBuf,
    pub pou_count: usize,
    pub source_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlcopenImportReport {
    pub project_root: PathBuf,
    pub written_sources: Vec<PathBuf>,
    pub imported_pous: usize,
    pub discovered_pous: usize,
    pub warnings: Vec<String>,
    pub unsupported_nodes: Vec<String>,
    pub preserved_vendor_extensions: Option<PathBuf>,
    pub migration_report_path: PathBuf,
    pub source_coverage_percent: f64,
    pub semantic_loss_percent: f64,
    pub detected_ecosystem: String,
    pub compatibility_coverage: PlcopenCompatibilityCoverage,
    pub unsupported_diagnostics: Vec<PlcopenUnsupportedDiagnostic>,
    pub applied_library_shims: Vec<PlcopenLibraryShimApplication>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlcopenMigrationReport {
    pub profile: String,
    pub namespace: String,
    pub source_xml: PathBuf,
    pub project_root: PathBuf,
    pub detected_ecosystem: String,
    pub discovered_pous: usize,
    pub importable_pous: usize,
    pub imported_pous: usize,
    pub skipped_pous: usize,
    pub source_coverage_percent: f64,
    pub semantic_loss_percent: f64,
    pub compatibility_coverage: PlcopenCompatibilityCoverage,
    pub unsupported_nodes: Vec<String>,
    pub unsupported_diagnostics: Vec<PlcopenUnsupportedDiagnostic>,
    pub applied_library_shims: Vec<PlcopenLibraryShimApplication>,
    pub warnings: Vec<String>,
    pub entries: Vec<PlcopenMigrationEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlcopenMigrationEntry {
    pub name: String,
    pub pou_type_raw: Option<String>,
    pub resolved_pou_type: Option<String>,
    pub status: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlcopenCompatibilityMatrixEntry {
    pub capability: &'static str,
    pub status: &'static str,
    pub notes: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlcopenCompatibilityCoverage {
    pub supported_items: usize,
    pub partial_items: usize,
    pub unsupported_items: usize,
    pub support_percent: f64,
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlcopenUnsupportedDiagnostic {
    pub code: String,
    pub severity: String,
    pub node: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pou: Option<String>,
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlcopenLibraryShimApplication {
    pub vendor: String,
    pub source_symbol: String,
    pub replacement_symbol: String,
    pub occurrences: usize,
    pub notes: String,
}

#[derive(Debug, Clone, Copy)]
struct VendorLibraryShim {
    source_symbol: &'static str,
    replacement_symbol: &'static str,
    notes: &'static str,
}

#[derive(Debug, Clone)]
struct LoadedSource {
    path: PathBuf,
    text: String,
}

#[derive(Debug, Clone)]
struct PouDecl {
    name: String,
    pou_type: PlcopenPouType,
    body: String,
    source: String,
    line: usize,
}

#[derive(Debug, Clone, Copy)]
enum PlcopenPouType {
    Program,
    Function,
    FunctionBlock,
}

impl PlcopenPouType {
    fn as_xml(self) -> &'static str {
        match self {
            Self::Program => "program",
            Self::Function => "function",
            Self::FunctionBlock => "functionBlock",
        }
    }

    fn from_xml(text: &str) -> Option<Self> {
        let normalized = text
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<String>();
        match normalized.as_str() {
            "program" | "prg" => Some(Self::Program),
            "function" | "fc" | "fun" => Some(Self::Function),
            "functionblock" | "fb" => Some(Self::FunctionBlock),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceMapEntry {
    name: String,
    pou_type: String,
    source: String,
    line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceMapPayload {
    profile: String,
    namespace: String,
    entries: Vec<SourceMapEntry>,
}

pub fn supported_profile() -> PlcopenProfile {
    PlcopenProfile {
        namespace: PLCOPEN_NAMESPACE,
        profile: PROFILE_NAME,
        version: "TC6 XML v2.0 (strict subset)",
        strict_subset: vec![
            "project/fileHeader/contentHeader",
            "types/pous/pou[pouType=program|function|functionBlock]",
            "pou/body/ST plain-text bodies",
            "addData/data[name=trust.sourceMap|trust.vendorExtensions]",
        ],
        unsupported_nodes: vec![
            "dataTypes",
            "instances/configurations/resources",
            "graphical bodies (FBD/LD/SFC)",
            "vendor-specific nodes (preserved via hooks, not interpreted)",
        ],
        compatibility_matrix: vec![
            PlcopenCompatibilityMatrixEntry {
                capability: "POU import/export: PROGRAM/FUNCTION/FUNCTION_BLOCK with ST body",
                status: "supported",
                notes: "Aliases such as PRG/FC/FB are normalized on import.",
            },
            PlcopenCompatibilityMatrixEntry {
                capability: "Source mapping metadata",
                status: "supported",
                notes: "Embedded addData trust.sourceMap + deterministic source-map sidecar JSON.",
            },
            PlcopenCompatibilityMatrixEntry {
                capability: "Vendor extension node preservation",
                status: "partial",
                notes: "Unknown addData/vendor fragments are preserved and re-injectable, but not semantically interpreted.",
            },
            PlcopenCompatibilityMatrixEntry {
                capability: "Vendor ecosystem migration heuristics",
                status: "partial",
                notes: "Detected ecosystems are advisory diagnostics for migration workflows, not semantic guarantees.",
            },
            PlcopenCompatibilityMatrixEntry {
                capability: "Vendor library compatibility shims (selected timer/edge aliases)",
                status: "partial",
                notes: "Import can normalize selected Siemens/Rockwell/Schneider/Mitsubishi aliases to IEC FB names and reports each shim application.",
            },
            PlcopenCompatibilityMatrixEntry {
                capability: "Graphical bodies (FBD/LD/SFC) and project-level runtime resources",
                status: "unsupported",
                notes: "Strict subset is ST-only and does not import graphical networks/configuration/resource execution models.",
            },
            PlcopenCompatibilityMatrixEntry {
                capability: "Vendor AOIs, advanced library semantics, and platform-specific pragmas",
                status: "unsupported",
                notes: "Shim catalog is intentionally narrow; unsupported content is reported in migration diagnostics and known-gaps docs.",
            },
        ],
        source_mapping: "Export writes deterministic source-map sidecar JSON and embeds trust.sourceMap in addData.",
        vendor_extension_hook:
            "Import preserves unknown addData/vendor nodes to plcopen.vendor-extensions.imported.xml; export re-injects plcopen.vendor-extensions.xml.",
        round_trip_limits: vec![
            "Round-trip guarantees preserve ST POU signatures (name/type/body intent) for strict-subset inputs.",
            "Round-trip does not preserve vendor formatting/layout, graphical networks, or runtime deployment metadata.",
            "Round-trip can rename output source files to sanitized unique names inside sources/.",
            "Round-trip may normalize selected vendor library symbols to IEC equivalents when shim rules apply during import.",
            "Round-trip preserves unknown vendor addData as opaque fragments, not executable semantics.",
        ],
        known_gaps: vec![
            "No import/export for SFC/LD/FBD bodies.",
            "No import of PLCopen instances/configurations/resources into runtime scheduling model.",
            "Vendor library shim coverage is limited to the published baseline alias catalog.",
            "No semantic translation for vendor-specific AOI/FB internal behavior beyond simple symbol remapping.",
            "No guaranteed equivalence for vendor pragmas, safety metadata, or online deployment tags.",
        ],
    }
}

pub fn export_project_to_xml(
    project_root: &Path,
    output_path: &Path,
) -> anyhow::Result<PlcopenExportReport> {
    let sources_root = project_root.join("sources");
    if !sources_root.is_dir() {
        anyhow::bail!(
            "invalid project folder '{}': missing sources/ directory",
            project_root.display()
        );
    }

    let sources = load_sources(project_root, &sources_root)?;
    if sources.is_empty() {
        anyhow::bail!("no ST sources found under {}", sources_root.display());
    }

    let mut warnings = Vec::new();
    let mut declarations = Vec::new();

    for source in &sources {
        let (mut declared, mut source_warnings) = extract_pou_declarations(source);
        declarations.append(&mut declared);
        warnings.append(&mut source_warnings);
    }

    if declarations.is_empty() {
        anyhow::bail!(
            "no PLCopen-compatible POU declarations discovered (supported: PROGRAM/FUNCTION/FUNCTION_BLOCK)"
        );
    }

    declarations.sort_by(|left, right| {
        left.pou_type
            .as_xml()
            .cmp(right.pou_type.as_xml())
            .then(left.name.cmp(&right.name))
            .then(left.source.cmp(&right.source))
    });

    let source_map = SourceMapPayload {
        profile: PROFILE_NAME.to_string(),
        namespace: PLCOPEN_NAMESPACE.to_string(),
        entries: declarations
            .iter()
            .map(|decl| SourceMapEntry {
                name: decl.name.clone(),
                pou_type: decl.pou_type.as_xml().to_string(),
                source: decl.source.clone(),
                line: decl.line,
            })
            .collect(),
    };
    let source_map_json = serde_json::to_string_pretty(&source_map)?;

    let project_name = project_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project");
    let generated_at = now_iso8601();

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<project xmlns=\"{}\" profile=\"{}\">\n",
        PLCOPEN_NAMESPACE, PROFILE_NAME
    ));
    xml.push_str(&format!(
        "  <fileHeader companyName=\"truST\" productName=\"trust-runtime\" productVersion=\"{}\" creationDateTime=\"{}\"/>\n",
        escape_xml_attr(env!("CARGO_PKG_VERSION")),
        escape_xml_attr(&generated_at)
    ));
    xml.push_str(&format!(
        "  <contentHeader name=\"{}\"/>\n",
        escape_xml_attr(project_name)
    ));
    xml.push_str("  <types>\n");
    xml.push_str("    <pous>\n");

    for decl in &declarations {
        xml.push_str(&format!(
            "      <pou name=\"{}\" pouType=\"{}\">\n",
            escape_xml_attr(&decl.name),
            decl.pou_type.as_xml()
        ));
        xml.push_str("        <body>\n");
        xml.push_str("          <ST><![CDATA[");
        xml.push_str(&escape_cdata(&decl.body));
        xml.push_str("]]></ST>\n");
        xml.push_str("        </body>\n");
        xml.push_str("      </pou>\n");
    }

    xml.push_str("    </pous>\n");
    xml.push_str("  </types>\n");
    xml.push_str("  <addData>\n");
    xml.push_str(&format!(
        "    <data name=\"{}\" handleUnknown=\"implementation\"><text><![CDATA[{}]]></text></data>\n",
        SOURCE_MAP_DATA_NAME,
        escape_cdata(&source_map_json)
    ));

    let vendor_hook_path = project_root.join(VENDOR_EXTENSION_HOOK_FILE);
    if vendor_hook_path.is_file() {
        let vendor_text = std::fs::read_to_string(&vendor_hook_path).with_context(|| {
            format!(
                "failed to read vendor extension hook '{}'",
                vendor_hook_path.display()
            )
        })?;
        xml.push_str(&format!(
            "    <data name=\"{}\" handleUnknown=\"implementation\"><text><![CDATA[{}]]></text></data>\n",
            VENDOR_EXT_DATA_NAME,
            escape_cdata(&vendor_text)
        ));
    }
    xml.push_str("  </addData>\n");
    xml.push_str("</project>\n");

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create PLCopen output directory '{}'",
                parent.display()
            )
        })?;
    }

    std::fs::write(output_path, xml)
        .with_context(|| format!("failed to write '{}'", output_path.display()))?;

    let source_map_path = output_path.with_extension("source-map.json");
    std::fs::write(&source_map_path, format!("{}\n", source_map_json)).with_context(|| {
        format!(
            "failed to write source-map sidecar '{}'",
            source_map_path.display()
        )
    })?;

    Ok(PlcopenExportReport {
        output_path: output_path.to_path_buf(),
        source_map_path,
        pou_count: declarations.len(),
        source_count: sources.len(),
        warnings,
    })
}

pub fn import_xml_to_project(
    xml_path: &Path,
    project_root: &Path,
) -> anyhow::Result<PlcopenImportReport> {
    let xml_text = std::fs::read_to_string(xml_path)
        .with_context(|| format!("failed to read PLCopen XML '{}'", xml_path.display()))?;
    let document = roxmltree::Document::parse(&xml_text)
        .with_context(|| format!("failed to parse PLCopen XML '{}'", xml_path.display()))?;

    let root = document.root_element();
    if root.tag_name().name() != "project" {
        anyhow::bail!(
            "invalid PLCopen XML: expected root <project>, found <{}>",
            root.tag_name().name()
        );
    }

    let mut warnings = Vec::new();
    let mut unsupported_nodes = Vec::new();
    let mut unsupported_diagnostics = Vec::new();
    let mut written_sources = Vec::new();
    let mut seen_files = HashSet::new();
    let mut migration_entries = Vec::new();
    let mut applied_shim_counts: BTreeMap<(String, String, String, String), usize> =
        BTreeMap::new();
    let mut discovered_pous = 0usize;
    let mut loss_warnings = 0usize;

    if let Some(namespace) = root.tag_name().namespace() {
        if namespace != PLCOPEN_NAMESPACE {
            warnings.push(format!(
                "unexpected namespace '{}'; expected '{}'",
                namespace, PLCOPEN_NAMESPACE
            ));
        }
    }

    inspect_unsupported_structure(
        root,
        &mut unsupported_nodes,
        &mut warnings,
        &mut unsupported_diagnostics,
    );

    let source_map = parse_embedded_source_map(root);
    let detected_ecosystem = detect_vendor_ecosystem(root, &xml_text);

    let sources_root = project_root.join("sources");
    std::fs::create_dir_all(&sources_root)
        .with_context(|| format!("failed to create '{}'", sources_root.display()))?;

    for pou in root
        .descendants()
        .filter(|node| is_element_named_ci(*node, "pou"))
    {
        discovered_pous += 1;
        let pou_name = extract_pou_name(pou);
        let entry_name = pou_name
            .clone()
            .unwrap_or_else(|| format!("unnamed_{discovered_pous}"));
        let pou_type_raw = attribute_ci(pou, "pouType").or_else(|| attribute_ci(pou, "type"));
        let resolved_pou_type = pou_type_raw.as_deref().and_then(PlcopenPouType::from_xml);
        let st_body = extract_st_body(pou);

        let Some(name) = pou_name else {
            warnings.push("skipping <pou> without name attribute".to_string());
            unsupported_diagnostics.push(unsupported_diagnostic(
                "PLCO201",
                "warning",
                "pou",
                "POU skipped because required name attribute is missing",
                None,
                "Skipped from import and counted in semantic-loss scoring",
            ));
            loss_warnings += 1;
            migration_entries.push(PlcopenMigrationEntry {
                name: entry_name,
                pou_type_raw,
                resolved_pou_type: resolved_pou_type.map(|kind| kind.as_xml().to_string()),
                status: "skipped".to_string(),
                reason: Some("missing name".to_string()),
            });
            continue;
        };

        let Some(pou_type_raw) = pou_type_raw else {
            warnings.push(format!("skipping pou '{}': missing pouType", name));
            unsupported_diagnostics.push(unsupported_diagnostic(
                "PLCO202",
                "warning",
                "pou",
                "POU skipped because pouType/type attribute is missing",
                Some(name.clone()),
                "Skipped from import and counted in semantic-loss scoring",
            ));
            loss_warnings += 1;
            migration_entries.push(PlcopenMigrationEntry {
                name,
                pou_type_raw: None,
                resolved_pou_type: None,
                status: "skipped".to_string(),
                reason: Some("missing pouType/type attribute".to_string()),
            });
            continue;
        };

        let Some(pou_type) = PlcopenPouType::from_xml(&pou_type_raw) else {
            warnings.push(format!(
                "skipping pou '{}': unsupported pouType '{}'",
                name, pou_type_raw
            ));
            unsupported_nodes.push(format!("pouType:{}", pou_type_raw));
            unsupported_diagnostics.push(unsupported_diagnostic(
                "PLCO203",
                "warning",
                format!("pouType:{pou_type_raw}"),
                format!("POU type '{pou_type_raw}' is outside the strict subset"),
                Some(name.clone()),
                "POU skipped; convert to PROGRAM/FUNCTION/FUNCTION_BLOCK or supported aliases",
            ));
            loss_warnings += 1;
            migration_entries.push(PlcopenMigrationEntry {
                name,
                pou_type_raw: Some(pou_type_raw),
                resolved_pou_type: None,
                status: "skipped".to_string(),
                reason: Some("unsupported pouType".to_string()),
            });
            continue;
        };

        let Some(body) = st_body else {
            warnings.push(format!("skipping pou '{}': missing body/ST", name));
            unsupported_diagnostics.push(unsupported_diagnostic(
                "PLCO204",
                "warning",
                "pou/body",
                "POU skipped because body/ST payload is missing",
                Some(name.clone()),
                "POU skipped; provide an ST body in PLCopen XML",
            ));
            loss_warnings += 1;
            migration_entries.push(PlcopenMigrationEntry {
                name,
                pou_type_raw: Some(pou_type_raw),
                resolved_pou_type: Some(pou_type.as_xml().to_string()),
                status: "skipped".to_string(),
                reason: Some("missing body/ST".to_string()),
            });
            continue;
        };

        let body = body.trim();
        if body.is_empty() {
            warnings.push(format!("skipping pou '{}': empty ST body", name));
            unsupported_diagnostics.push(unsupported_diagnostic(
                "PLCO205",
                "warning",
                "pou/body/ST",
                "POU skipped because ST body is empty",
                Some(name.clone()),
                "POU skipped; provide non-empty ST source text",
            ));
            loss_warnings += 1;
            migration_entries.push(PlcopenMigrationEntry {
                name,
                pou_type_raw: Some(pou_type_raw),
                resolved_pou_type: Some(pou_type.as_xml().to_string()),
                status: "skipped".to_string(),
                reason: Some("empty ST body".to_string()),
            });
            continue;
        }

        let mut file_name = sanitize_filename(&name);
        if file_name.is_empty() {
            file_name = "unnamed".to_string();
        }
        let mut candidate = sources_root.join(format!("{file_name}.st"));
        let mut duplicate_index = 2usize;
        while !seen_files.insert(candidate.clone()) {
            candidate = sources_root.join(format!("{file_name}_{duplicate_index}.st"));
            duplicate_index += 1;
        }

        let normalized_body = normalize_body_text(body);
        let (shimmed_body, shim_applications) =
            apply_vendor_library_shims(&normalized_body, &detected_ecosystem);
        for application in shim_applications {
            warnings.push(format!(
                "applied vendor library shim in pou '{}': {} -> {} ({} occurrence(s))",
                name,
                application.source_symbol,
                application.replacement_symbol,
                application.occurrences
            ));
            unsupported_diagnostics.push(unsupported_diagnostic(
                "PLCO301",
                "info",
                format!("vendor-shim:{}", application.source_symbol),
                format!(
                    "Vendor library shim mapped '{}' to '{}'",
                    application.source_symbol, application.replacement_symbol
                ),
                Some(name.clone()),
                application.notes.clone(),
            ));
            let key = (
                application.vendor,
                application.source_symbol,
                application.replacement_symbol,
                application.notes,
            );
            *applied_shim_counts.entry(key).or_insert(0) += application.occurrences;
        }

        std::fs::write(&candidate, shimmed_body)
            .with_context(|| format!("failed to write '{}'", candidate.display()))?;
        written_sources.push(candidate);

        migration_entries.push(PlcopenMigrationEntry {
            name: name.clone(),
            pou_type_raw: Some(pou_type_raw),
            resolved_pou_type: Some(pou_type.as_xml().to_string()),
            status: "imported".to_string(),
            reason: None,
        });

        if let Some(entry) = source_map.as_ref().and_then(|map| {
            map.entries
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(&name))
        }) {
            warnings.push(format!(
                "source map: pou '{}' originated from {}:{}",
                name, entry.source, entry.line
            ));
        }
    }

    if discovered_pous == 0 {
        warnings.push("no <pou> nodes discovered in input XML".to_string());
        unsupported_diagnostics.push(unsupported_diagnostic(
            "PLCO206",
            "warning",
            "types/pous",
            "Input XML does not contain importable <pou> elements",
            None,
            "Provide PLCopen ST POUs under project/types/pous",
        ));
        loss_warnings += 1;
    }

    let imported_pous = written_sources.len();
    let importable_pous = migration_entries
        .iter()
        .filter(|entry| entry.status == "imported")
        .count();
    let skipped_pous = discovered_pous.saturating_sub(imported_pous);
    let source_coverage_percent = calculate_source_coverage(imported_pous, discovered_pous);
    let semantic_loss_percent = calculate_semantic_loss(
        imported_pous,
        discovered_pous,
        unsupported_nodes.len(),
        loss_warnings,
    );
    let applied_library_shims = applied_shim_counts
        .into_iter()
        .map(
            |((vendor, source_symbol, replacement_symbol, notes), occurrences)| {
                PlcopenLibraryShimApplication {
                    vendor,
                    source_symbol,
                    replacement_symbol,
                    occurrences,
                    notes,
                }
            },
        )
        .collect::<Vec<_>>();
    let shimmed_occurrences = applied_library_shims
        .iter()
        .map(|entry| entry.occurrences)
        .sum::<usize>();
    let compatibility_coverage = calculate_compatibility_coverage(
        imported_pous,
        skipped_pous,
        unsupported_nodes.len(),
        shimmed_occurrences,
    );

    let preserved_vendor_extensions =
        preserve_vendor_extensions(root, &xml_text, project_root, &mut warnings)?;
    let migration_report = PlcopenMigrationReport {
        profile: PROFILE_NAME.to_string(),
        namespace: root
            .tag_name()
            .namespace()
            .unwrap_or(PLCOPEN_NAMESPACE)
            .to_string(),
        source_xml: xml_path.to_path_buf(),
        project_root: project_root.to_path_buf(),
        detected_ecosystem: detected_ecosystem.clone(),
        discovered_pous,
        importable_pous,
        imported_pous,
        skipped_pous,
        source_coverage_percent,
        semantic_loss_percent,
        compatibility_coverage: compatibility_coverage.clone(),
        unsupported_nodes: unsupported_nodes.clone(),
        unsupported_diagnostics: unsupported_diagnostics.clone(),
        applied_library_shims: applied_library_shims.clone(),
        warnings: warnings.clone(),
        entries: migration_entries,
    };
    let migration_report_path = write_migration_report(project_root, &migration_report)?;

    if written_sources.is_empty() {
        anyhow::bail!(
            "no importable PLCopen ST POUs found in {} (migration report: {})",
            xml_path.display(),
            migration_report_path.display()
        );
    }

    Ok(PlcopenImportReport {
        project_root: project_root.to_path_buf(),
        imported_pous,
        discovered_pous,
        written_sources,
        warnings,
        unsupported_nodes,
        preserved_vendor_extensions,
        migration_report_path,
        source_coverage_percent,
        semantic_loss_percent,
        detected_ecosystem,
        compatibility_coverage,
        unsupported_diagnostics,
        applied_library_shims,
    })
}

fn load_sources(project_root: &Path, sources_root: &Path) -> anyhow::Result<Vec<LoadedSource>> {
    let mut paths = BTreeSet::new();
    for pattern in ["**/*.st", "**/*.ST", "**/*.pou", "**/*.POU"] {
        let glob_pattern = format!("{}/{}", sources_root.display(), pattern);
        for entry in glob::glob(&glob_pattern)
            .with_context(|| format!("invalid source glob '{}'", glob_pattern))?
        {
            paths.insert(entry?);
        }
    }

    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read source '{}'", path.display()))?;
        let relative = path
            .strip_prefix(project_root)
            .map_or_else(|_| path.clone(), Path::to_path_buf);
        sources.push(LoadedSource {
            path: relative,
            text,
        });
    }
    Ok(sources)
}

fn extract_pou_declarations(source: &LoadedSource) -> (Vec<PouDecl>, Vec<String>) {
    let mut declarations = Vec::new();
    let mut warnings = Vec::new();

    let parsed = parser::parse(&source.text);
    let syntax = parsed.syntax();

    for node in syntax.children() {
        let Some(pou_type) = node_to_pou_type(&node) else {
            if is_unsupported_top_level(&node) {
                let line = line_for_node(&source.text, &node);
                warnings.push(format!(
                    "{}:{} unsupported top-level node '{:?}' skipped for PLCopen strict subset",
                    source.path.display(),
                    line,
                    node.kind()
                ));
            }
            continue;
        };

        let Some(name) = declaration_name(&node) else {
            continue;
        };

        if is_test_pou(&node) {
            let line = line_for_node(&source.text, &node);
            warnings.push(format!(
                "{}:{} test POU '{}' exported as standard '{}'",
                source.path.display(),
                line,
                name,
                pou_type.as_xml()
            ));
        }

        let line = line_for_node(&source.text, &node);
        declarations.push(PouDecl {
            name,
            pou_type,
            body: normalize_body_text(node.text().to_string()),
            source: source.path.display().to_string(),
            line,
        });
    }

    (declarations, warnings)
}

fn node_to_pou_type(node: &SyntaxNode) -> Option<PlcopenPouType> {
    match node.kind() {
        SyntaxKind::Program => Some(PlcopenPouType::Program),
        SyntaxKind::Function => Some(PlcopenPouType::Function),
        SyntaxKind::FunctionBlock => Some(PlcopenPouType::FunctionBlock),
        _ => None,
    }
}

fn is_unsupported_top_level(node: &SyntaxNode) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::Class
            | SyntaxKind::Interface
            | SyntaxKind::Namespace
            | SyntaxKind::Configuration
            | SyntaxKind::TypeDecl
            | SyntaxKind::Action
    )
}

fn is_test_pou(node: &SyntaxNode) -> bool {
    first_non_trivia_token(node).is_some_and(|kind| {
        matches!(
            kind,
            SyntaxKind::KwTestProgram | SyntaxKind::KwTestFunctionBlock
        )
    })
}

fn first_non_trivia_token(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| !token.kind().is_trivia())
        .map(|token| token.kind())
}

fn declaration_name(node: &SyntaxNode) -> Option<String> {
    node.children()
        .find(|child| child.kind() == SyntaxKind::Name)
        .map(|name| name.text().to_string().trim().to_string())
        .filter(|text| !text.is_empty())
}

fn line_for_node(source: &str, node: &SyntaxNode) -> usize {
    let offset = node
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| !token.kind().is_trivia())
        .map(|token| usize::from(token.text_range().start()))
        .unwrap_or(0);
    source[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{timestamp}Z")
}

fn escape_xml_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
}

fn escape_cdata(value: &str) -> String {
    value.replace("]]>", "]]]]><![CDATA[>")
}

fn normalize_body_text(text: impl Into<String>) -> String {
    let mut normalized = text.into().replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    normalized
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
}

#[cfg(test)]
fn is_element_named(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn is_element_named_ci(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name)
}

fn attribute_ci(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    node.attributes()
        .find(|attribute| attribute.name().eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value().to_string())
}

fn extract_pou_name(node: roxmltree::Node<'_, '_>) -> Option<String> {
    attribute_ci(node, "name")
        .or_else(|| attribute_ci(node, "pouName"))
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            node.children()
                .find(|child| is_element_named_ci(*child, "name"))
                .and_then(extract_text_content)
        })
}

fn extract_st_body(node: roxmltree::Node<'_, '_>) -> Option<String> {
    let body = node
        .children()
        .find(|child| is_element_named_ci(*child, "body"))?;
    for preferred in ["ST", "st", "text", "Text", "xhtml"] {
        if let Some(candidate) = body
            .descendants()
            .find(|entry| is_element_named_ci(*entry, preferred))
            .and_then(extract_text_content)
        {
            return Some(candidate);
        }
    }
    extract_text_content(body)
}

fn extract_text_content(node: roxmltree::Node<'_, '_>) -> Option<String> {
    let text = node
        .descendants()
        .filter(|entry| entry.is_text())
        .filter_map(|entry| entry.text())
        .collect::<String>();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn vendor_library_shims_for_ecosystem(ecosystem: &str) -> &'static [VendorLibraryShim] {
    match ecosystem {
        "siemens-tia" => SIEMENS_LIBRARY_SHIMS,
        "rockwell-studio5000" => ROCKWELL_LIBRARY_SHIMS,
        "schneider-ecostruxure" | "codesys" => SCHNEIDER_LIBRARY_SHIMS,
        "mitsubishi-gxworks3" => MITSUBISHI_LIBRARY_SHIMS,
        _ => &[],
    }
}

fn apply_vendor_library_shims(
    body: &str,
    ecosystem: &str,
) -> (String, Vec<PlcopenLibraryShimApplication>) {
    let shims = vendor_library_shims_for_ecosystem(ecosystem);
    if shims.is_empty() {
        return (body.to_string(), Vec::new());
    }

    let tokens = lex(body);
    if tokens.is_empty() {
        return (body.to_string(), Vec::new());
    }

    let mut output = String::with_capacity(body.len());
    let mut cursor = 0usize;
    let mut counts: BTreeMap<(String, String, String, String), usize> = BTreeMap::new();

    for (index, token) in tokens.iter().enumerate() {
        let start = usize::from(token.range.start());
        let end = usize::from(token.range.end());
        if start > cursor {
            output.push_str(&body[cursor..start]);
        }

        let token_text = &body[start..end];
        if token.kind == TokenKind::Ident {
            if let Some(shim) = match_library_shim(shims, token_text, &tokens, index) {
                output.push_str(shim.replacement_symbol);
                let key = (
                    ecosystem.to_string(),
                    shim.source_symbol.to_string(),
                    shim.replacement_symbol.to_string(),
                    shim.notes.to_string(),
                );
                *counts.entry(key).or_insert(0) += 1;
            } else {
                output.push_str(token_text);
            }
        } else {
            output.push_str(token_text);
        }
        cursor = end;
    }

    if cursor < body.len() {
        output.push_str(&body[cursor..]);
    }

    let applications = counts
        .into_iter()
        .map(
            |((vendor, source_symbol, replacement_symbol, notes), occurrences)| {
                PlcopenLibraryShimApplication {
                    vendor,
                    source_symbol,
                    replacement_symbol,
                    occurrences,
                    notes,
                }
            },
        )
        .collect();
    (output, applications)
}

fn match_library_shim<'a>(
    shims: &'a [VendorLibraryShim],
    token_text: &str,
    tokens: &[trust_syntax::lexer::Token],
    index: usize,
) -> Option<&'a VendorLibraryShim> {
    let upper = token_text.to_ascii_uppercase();
    let shim = shims
        .iter()
        .find(|candidate| candidate.source_symbol == upper)?;

    let previous = previous_non_trivia_token_kind(tokens, index);
    let next = next_non_trivia_token_kind(tokens, index);
    if previous == Some(TokenKind::Dot) {
        return None;
    }

    let type_position = matches!(
        previous,
        Some(TokenKind::Colon)
            | Some(TokenKind::KwOf)
            | Some(TokenKind::KwExtends)
            | Some(TokenKind::KwRefTo)
    );
    let call_position = next == Some(TokenKind::LParen);
    if type_position || call_position {
        Some(shim)
    } else {
        None
    }
}

fn previous_non_trivia_token_kind(
    tokens: &[trust_syntax::lexer::Token],
    index: usize,
) -> Option<TokenKind> {
    let mut current = index;
    while current > 0 {
        current -= 1;
        let kind = tokens[current].kind;
        if !kind.is_trivia() {
            return Some(kind);
        }
    }
    None
}

fn next_non_trivia_token_kind(
    tokens: &[trust_syntax::lexer::Token],
    index: usize,
) -> Option<TokenKind> {
    let mut current = index + 1;
    while current < tokens.len() {
        let kind = tokens[current].kind;
        if !kind.is_trivia() {
            return Some(kind);
        }
        current += 1;
    }
    None
}

fn calculate_source_coverage(imported: usize, discovered: usize) -> f64 {
    if discovered == 0 {
        return 0.0;
    }
    round_percent((imported as f64 / discovered as f64) * 100.0)
}

fn calculate_semantic_loss(
    imported: usize,
    discovered: usize,
    unsupported_nodes: usize,
    loss_warnings: usize,
) -> f64 {
    if discovered == 0 {
        return 100.0;
    }

    let skipped = discovered.saturating_sub(imported);
    let skipped_ratio = skipped as f64 / discovered as f64;
    let unsupported_ratio =
        unsupported_nodes as f64 / (unsupported_nodes as f64 + discovered as f64);
    let warning_ratio = (loss_warnings as f64 / (discovered as f64 * 2.0)).min(1.0);

    round_percent((skipped_ratio * 70.0) + (unsupported_ratio * 20.0) + (warning_ratio * 10.0))
}

fn calculate_compatibility_coverage(
    imported_pous: usize,
    skipped_pous: usize,
    unsupported_nodes: usize,
    shimmed_occurrences: usize,
) -> PlcopenCompatibilityCoverage {
    let supported_items = imported_pous;
    let partial_items = unsupported_nodes + shimmed_occurrences;
    let unsupported_items = skipped_pous;
    let total = supported_items + partial_items + unsupported_items;
    let support_percent = if total == 0 {
        0.0
    } else {
        round_percent((supported_items as f64 / total as f64) * 100.0)
    };
    let verdict = if total == 0 {
        "none"
    } else if unsupported_items == 0 && partial_items == 0 {
        "full"
    } else if supported_items > 0 {
        "partial"
    } else {
        "low"
    };
    PlcopenCompatibilityCoverage {
        supported_items,
        partial_items,
        unsupported_items,
        support_percent,
        verdict: verdict.to_string(),
    }
}

fn unsupported_diagnostic(
    code: &str,
    severity: &str,
    node: impl Into<String>,
    message: impl Into<String>,
    pou: Option<String>,
    action: impl Into<String>,
) -> PlcopenUnsupportedDiagnostic {
    PlcopenUnsupportedDiagnostic {
        code: code.to_string(),
        severity: severity.to_string(),
        node: node.into(),
        message: message.into(),
        pou,
        action: action.into(),
    }
}

fn round_percent(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn detect_vendor_ecosystem(root: roxmltree::Node<'_, '_>, xml_text: &str) -> String {
    let mut hints = String::new();
    for node in root.descendants().filter(|node| node.is_element()) {
        for attribute in node.attributes() {
            hints.push_str(attribute.value());
            hints.push(' ');
        }
        if is_element_named_ci(node, "data") {
            if let Some(name) = attribute_ci(node, "name") {
                hints.push_str(&name);
                hints.push(' ');
            }
        }
    }
    hints.push_str(xml_text);
    let normalized = hints.to_ascii_lowercase();

    if normalized.contains("twincat") || normalized.contains("beckhoff") {
        "beckhoff-twincat".to_string()
    } else if normalized.contains("schneider")
        || normalized.contains("ecostruxure")
        || normalized.contains("unity pro")
        || normalized.contains("control expert")
    {
        "schneider-ecostruxure".to_string()
    } else if normalized.contains("codesys")
        || normalized.contains("3s-smart")
        || normalized.contains("machine expert")
    {
        "codesys".to_string()
    } else if normalized.contains("siemens")
        || normalized.contains("tia portal")
        || normalized.contains("step7")
    {
        "siemens-tia".to_string()
    } else if normalized.contains("rockwell")
        || normalized.contains("studio 5000")
        || normalized.contains("allen-bradley")
    {
        "rockwell-studio5000".to_string()
    } else if normalized.contains("mitsubishi")
        || normalized.contains("gx works")
        || normalized.contains("gxworks")
        || normalized.contains("melsoft")
    {
        "mitsubishi-gxworks3".to_string()
    } else {
        "generic-plcopen".to_string()
    }
}

fn write_migration_report(
    project_root: &Path,
    report: &PlcopenMigrationReport,
) -> anyhow::Result<PathBuf> {
    let report_path = project_root.join(MIGRATION_REPORT_FILE);
    if let Some(parent) = report_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create migration report directory '{}'",
                parent.display()
            )
        })?;
    }
    let json = serde_json::to_string_pretty(report)?;
    std::fs::write(&report_path, format!("{json}\n")).with_context(|| {
        format!(
            "failed to write PLCopen migration report '{}'",
            report_path.display()
        )
    })?;
    Ok(report_path)
}

fn inspect_unsupported_structure(
    root: roxmltree::Node<'_, '_>,
    unsupported_nodes: &mut Vec<String>,
    warnings: &mut Vec<String>,
    unsupported_diagnostics: &mut Vec<PlcopenUnsupportedDiagnostic>,
) {
    for child in root.children().filter(|child| child.is_element()) {
        let name = child.tag_name().name();
        if !matches!(
            name.to_ascii_lowercase().as_str(),
            "fileheader" | "contentheader" | "types" | "adddata"
        ) {
            unsupported_nodes.push(name.to_string());
            warnings.push(format!(
                "unsupported PLCopen node '<{}>' preserved as metadata only",
                name
            ));
            unsupported_diagnostics.push(unsupported_diagnostic(
                "PLCO101",
                "warning",
                name,
                format!("Unsupported top-level PLCopen node '<{}>'", name),
                None,
                "Preserved as metadata only; not imported into runtime semantics",
            ));
        }
        if name.eq_ignore_ascii_case("types") {
            for type_child in child.children().filter(|entry| entry.is_element()) {
                let type_name = type_child.tag_name().name();
                if !type_name.eq_ignore_ascii_case("pous") {
                    unsupported_nodes.push(format!("types/{}", type_name));
                    warnings.push(format!(
                        "unsupported PLCopen node '<types>/<{}>' skipped (strict subset)",
                        type_name
                    ));
                    unsupported_diagnostics.push(unsupported_diagnostic(
                        "PLCO102",
                        "warning",
                        format!("types/{type_name}"),
                        format!("Unsupported PLCopen <types>/<{}> section", type_name),
                        None,
                        "Skipped in strict subset; migrate supported POUs manually",
                    ));
                }
            }
        }
    }
}

fn parse_embedded_source_map(root: roxmltree::Node<'_, '_>) -> Option<SourceMapPayload> {
    let payload = root
        .descendants()
        .find(|node| {
            is_element_named_ci(*node, "data")
                && attribute_ci(*node, "name").is_some_and(|name| name == SOURCE_MAP_DATA_NAME)
        })
        .and_then(|node| {
            node.children()
                .find(|child| is_element_named_ci(*child, "text"))
                .and_then(extract_text_content)
        })?;
    serde_json::from_str::<SourceMapPayload>(&payload).ok()
}

fn preserve_vendor_extensions(
    root: roxmltree::Node<'_, '_>,
    xml_text: &str,
    project_root: &Path,
    warnings: &mut Vec<String>,
) -> anyhow::Result<Option<PathBuf>> {
    let mut preserved = Vec::new();

    for node in root.descendants().filter(|node| {
        is_element_named_ci(*node, "data")
            && attribute_ci(*node, "name").is_none_or(|name| name != SOURCE_MAP_DATA_NAME)
    }) {
        let range = node.range();
        if let Some(slice) = xml_text.get(range) {
            preserved.push(slice.trim().to_string());
        }
    }

    if preserved.is_empty() {
        return Ok(None);
    }

    let output = project_root.join(IMPORTED_VENDOR_EXTENSION_FILE);
    let mut content = String::from("<vendorExtensions>\n");
    for fragment in preserved {
        content.push_str("  ");
        content.push_str(&fragment);
        content.push('\n');
    }
    content.push_str("</vendorExtensions>\n");
    std::fs::write(&output, content)
        .with_context(|| format!("failed to write '{}'", output.display()))?;
    warnings.push(format!(
        "preserved vendor extension nodes in {}",
        output.display()
    ));
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("trust-runtime-{prefix}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("create temp directory");
        dir
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, content).expect("write file");
    }

    fn pou_signatures(xml: &str) -> Vec<(String, String, String)> {
        let doc = roxmltree::Document::parse(xml).expect("parse XML");
        let mut items = doc
            .descendants()
            .filter(|node| is_element_named(*node, "pou"))
            .filter_map(|pou| {
                let name = pou.attribute("name")?.to_string();
                let pou_type = pou.attribute("pouType")?.to_string();
                let body = pou
                    .children()
                    .find(|child| is_element_named(*child, "body"))
                    .and_then(|body| {
                        body.children()
                            .find(|child| is_element_named(*child, "ST"))
                            .and_then(|st| st.text())
                    })
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string();
                Some((name, pou_type, body))
            })
            .collect::<Vec<_>>();
        items.sort();
        items
    }

    #[test]
    fn round_trip_export_import_export_preserves_pou_subset() {
        let source_project = temp_dir("plcopen-roundtrip-src");
        write(
            &source_project.join("sources/main.st"),
            r#"
PROGRAM Main
VAR
    speed : REAL := 42.5;
END_VAR
END_PROGRAM
"#,
        );
        write(
            &source_project.join("sources/calc.st"),
            r#"
FUNCTION Calc : INT
VAR_INPUT
    A : INT;
END_VAR
Calc := A + 1;
END_FUNCTION
"#,
        );

        let xml_a = source_project.join("build/plcopen.xml");
        let export_a = export_project_to_xml(&source_project, &xml_a).expect("export A");
        assert_eq!(export_a.pou_count, 2);
        assert!(export_a.source_map_path.is_file());

        let import_project = temp_dir("plcopen-roundtrip-import");
        let import = import_xml_to_project(&xml_a, &import_project).expect("import");
        assert_eq!(import.imported_pous, 2);
        assert_eq!(import.discovered_pous, 2);
        assert!(import.migration_report_path.is_file());
        assert_eq!(import.source_coverage_percent, 100.0);
        assert_eq!(import.semantic_loss_percent, 0.0);

        let xml_b = import_project.join("build/plcopen.xml");
        let export_b = export_project_to_xml(&import_project, &xml_b).expect("export B");
        assert_eq!(export_b.pou_count, 2);

        let a_text = std::fs::read_to_string(&xml_a).expect("read xml A");
        let b_text = std::fs::read_to_string(&xml_b).expect("read xml B");
        assert_eq!(pou_signatures(&a_text), pou_signatures(&b_text));

        let _ = std::fs::remove_dir_all(source_project);
        let _ = std::fs::remove_dir_all(import_project);
    }

    #[test]
    fn import_reports_unsupported_nodes_and_preserves_vendor_extensions() {
        let project = temp_dir("plcopen-import-unsupported");
        let xml_path = project.join("input.xml");
        write(
            &xml_path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://www.plcopen.org/xml/tc6_0200">
  <types>
    <dataTypes>
      <dataType name="POINT"/>
    </dataTypes>
    <pous>
      <pou name="Main" pouType="program">
        <body>
          <ST><![CDATA[
PROGRAM Main
VAR
  speed : REAL := 10.0;
END_VAR
END_PROGRAM
]]></ST>
        </body>
      </pou>
    </pous>
  </types>
  <addData>
    <data name="vendor.raw"><text><![CDATA[<vendorNode id="1"/>]]></text></data>
  </addData>
</project>
"#,
        );

        let report = import_xml_to_project(&xml_path, &project).expect("import XML");
        assert_eq!(report.imported_pous, 1);
        assert_eq!(report.discovered_pous, 1);
        assert!(!report.unsupported_nodes.is_empty());
        assert!(report
            .unsupported_nodes
            .iter()
            .any(|entry| entry.contains("types/dataTypes")));
        assert!(report.migration_report_path.is_file());
        assert!(report.source_coverage_percent > 0.0);
        assert!(report.semantic_loss_percent > 0.0);
        assert_eq!(report.compatibility_coverage.verdict, "partial");
        assert!(report
            .unsupported_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "PLCO102"));
        let source = std::fs::read_to_string(&report.written_sources[0]).expect("read source");
        assert!(source.contains("PROGRAM Main"));
        let vendor = report
            .preserved_vendor_extensions
            .expect("vendor extension path");
        let vendor_text = std::fs::read_to_string(vendor).expect("read vendor ext");
        assert!(vendor_text.contains("vendor.raw"));

        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn import_applies_siemens_library_shims_and_reports_them() {
        let project = temp_dir("plcopen-import-siemens-shims");
        let xml_path = project.join("siemens.xml");
        write(
            &xml_path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://www.plcopen.org/xml/tc6_0200">
  <fileHeader companyName="Siemens AG" productName="TIA Portal V18" />
  <types>
    <pous>
      <pou name="MainOb1" pouType="PRG">
        <body>
          <ST><![CDATA[
PROGRAM MainOb1
VAR
    PulseTimer : SFB3;
    DelayTimer : SFB4;
END_VAR
PulseTimer(IN := TRUE, PT := T#200ms);
DelayTimer(IN := PulseTimer.Q, PT := T#2s);
END_PROGRAM
]]></ST>
        </body>
      </pou>
    </pous>
  </types>
</project>
"#,
        );

        let report = import_xml_to_project(&xml_path, &project).expect("import XML");
        assert_eq!(report.detected_ecosystem, "siemens-tia");
        assert!(!report.applied_library_shims.is_empty());
        assert!(report
            .applied_library_shims
            .iter()
            .any(|entry| entry.source_symbol == "SFB3" && entry.replacement_symbol == "TP"));
        assert!(report
            .applied_library_shims
            .iter()
            .any(|entry| entry.source_symbol == "SFB4" && entry.replacement_symbol == "TON"));
        assert!(report
            .unsupported_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "PLCO301"));

        let source = std::fs::read_to_string(&report.written_sources[0]).expect("read source");
        assert!(source.contains("PulseTimer : TP;"));
        assert!(source.contains("DelayTimer : TON;"));
        assert!(!source.contains("SFB3"));
        assert!(!source.contains("SFB4"));

        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn library_shim_rewrites_type_and_call_sites_only() {
        let body = r#"
PROGRAM Main
VAR
    SFB4 : BOOL := FALSE;
    DelayTimer : SFB4;
END_VAR
SFB4 := TRUE;
DelayTimer(IN := SFB4, PT := T#1s);
END_PROGRAM
"#;

        let (shimmed, applied) = apply_vendor_library_shims(body, "siemens-tia");
        assert_eq!(applied.len(), 1);
        assert!(shimmed.contains("SFB4 : BOOL := FALSE;"));
        assert!(shimmed.contains("DelayTimer : TON;"));
        assert!(shimmed.contains("SFB4 := TRUE;"));
    }

    #[test]
    fn import_rejects_malformed_xml() {
        let project = temp_dir("plcopen-malformed");
        let xml_path = project.join("broken.xml");
        write(&xml_path, "<project><types><pous><pou>");

        let result = import_xml_to_project(&xml_path, &project);
        assert!(result.is_err(), "malformed XML must return error");

        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn export_reinjects_vendor_extension_hook_file() {
        let project = temp_dir("plcopen-export-vendor-hook");
        write(
            &project.join("sources/main.st"),
            r#"
PROGRAM Main
END_PROGRAM
"#,
        );
        write(
            &project.join(VENDOR_EXTENSION_HOOK_FILE),
            r#"<vendorData source="external"/>"#,
        );

        let output = project.join("out/plcopen.xml");
        export_project_to_xml(&project, &output).expect("export XML");
        let text = std::fs::read_to_string(output).expect("read output XML");
        assert!(text.contains(VENDOR_EXT_DATA_NAME));
        assert!(text.contains("vendorData"));

        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn profile_declares_strict_subset_contract() {
        let profile = supported_profile();
        assert_eq!(profile.namespace, PLCOPEN_NAMESPACE);
        assert_eq!(profile.profile, PROFILE_NAME);
        assert!(profile
            .strict_subset
            .iter()
            .any(|item| item.contains("types/pous/pou")));
        assert!(profile
            .compatibility_matrix
            .iter()
            .any(|entry| entry.status == "supported"));
        assert!(profile.compatibility_matrix.iter().any(|entry| {
            entry.status == "partial" && entry.capability.contains("compatibility shims")
        }));
        assert!(!profile.round_trip_limits.is_empty());
        assert!(!profile.known_gaps.is_empty());
    }
}
