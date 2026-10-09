//! The syntactic tier: a file's definitions as its tree-sitter grammar
//! parses them, each with its kind, its exact lines, the line its name is
//! on and what it's in (`impl Session`'s `stop` is `Session::stop`).
//!
//! Each language is walked by rules of its own, not its grammar's tags
//! query, which leaves out what the wiki links most: fields, constants and
//! variants, and what a method is in. The walk goes into what holds
//! definitions (modules, classes, impls, structs' bodies) but not into
//! functions' bodies, whose locals no prose names. On the way, it reads what
//! makes a name the user types: a Rust field's serde key (`#[serde(rename)]`,
//! `rename_all`) and clap flag (`#[arg(long)]`), a clap variant's
//! subcommand, a Go field's struct tag, and the flags and subcommands Go's
//! cobra and flag packages, Python's argparse and click, and JavaScript's
//! commander define in calls.
//!
//! Rust, Go, Python, TypeScript and JavaScript (TSX's grammar reads both),
//! Java, C and C++ (whose grammar reads C's headers too) are compiled in,
//! about 8 MB of the binary; C#, Kotlin, Swift, Ruby, PHP and shell's
//! would be 17 MB more, so they're read by their keywords
//! ([`super::keywords`]).

use super::DefKind;
use super::files::Lang;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use tree_sitter::{Language, Node, Parser};

/// What a file defines, as its grammar or its keywords read it: kept in the
/// cache by its blob, since it depends on nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outline {
    /// The package the file says it's in, which what it defines is named
    /// under: Go's `package`, Java's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package: Vec<String>,
    pub defs: Vec<Found>,
    /// How many lines it has.
    pub lines: u32,
}

/// A definition in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    /// What it's in, then its own name: `["Session", "stop"]`.
    pub segments: Vec<String>,
    pub kind: DefKind,
    /// Its first and last lines, from 1.
    pub start: u32,
    pub end: u32,
    /// The line its name is on, which an indexer's definition is matched
    /// by.
    pub line: u32,
    /// The other names it goes by: a field's key in a config file and its
    /// flag on a command line (`--wait`), a subcommand's word
    /// (`restart-server`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    /// A field's type, or a variant's fields', as written: what a config
    /// key's table or a subcommand's own subcommands are followed by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    /// Whether it's a test's.
    #[serde(default, skip_serializing_if = "is_false")]
    pub test: bool,
    /// Whether it's only declared here, defined elsewhere or nowhere: a C
    /// prototype, a trait's method with no body, `mod x;`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub decl: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// The grammar compiled in for `lang`, if there is one.
pub fn compiled(lang: Lang) -> Option<Language> {
    Some(match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TypeScript | Lang::JavaScript => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::C => tree_sitter_c::LANGUAGE.into(),
        Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        _ => return None,
    })
}

/// What the file `source`, in `lang`, defines, read by its grammar with
/// `parser`; `None` when there's no grammar for it or it can't be parsed.
/// What's a test's is told by what's in the file (`#[test]`, `mod tests`);
/// a whole file of tests is told by its path, which isn't the blob's.
pub fn outline(lang: Lang, source: &[u8], parser: &mut Parser) -> Option<Outline> {
    parser.set_language(&compiled(lang)?).ok()?;
    let tree = parser.parse(source, None)?;
    let mut walk = Walk {
        lang,
        src: source,
        defs: Vec::new(),
        package: Vec::new(),
    };
    let root = tree.root_node();
    walk.children(root, &Scope::default());
    walk.calls(root);
    Some(Outline {
        package: walk.package,
        defs: walk.defs,
        lines: lines(source),
    })
}

/// How many lines `source` has.
pub fn lines(source: &[u8]) -> u32 {
    let newlines = source.iter().filter(|&&byte| byte == b'\n').count();
    let last = usize::from(source.last().is_some_and(|&byte| byte != b'\n'));
    u32::try_from(newlines + last).unwrap_or(u32::MAX)
}

/// What a definition is in, as the walk goes down.
#[derive(Debug, Clone, Default)]
struct Scope {
    /// The names of what it's in.
    path: Vec<String>,
    /// Inside a class, an impl, a trait or the like, where a function is a
    /// method.
    members: bool,
    test: bool,
    /// Rust: inside a struct or an enum that derives clap's `Parser`,
    /// `Args` or `Subcommand`.
    clap: bool,
    /// Rust: serde's `rename_all` on the struct or enum it's in.
    rename_all: Option<String>,
}

impl Scope {
    /// The scope inside `name`.
    fn under(&self, name: &str) -> Scope {
        let mut path = self.path.clone();
        path.push(name.to_string());
        Scope {
            path,
            members: true,
            test: self.test,
            clap: false,
            rename_all: None,
        }
    }
}

struct Walk<'s> {
    lang: Lang,
    src: &'s [u8],
    defs: Vec<Found>,
    package: Vec<String>,
}

impl<'s> Walk<'s> {
    fn text(&self, node: Node) -> &'s str {
        node.utf8_text(self.src).unwrap_or("")
    }

    /// The text of `node`'s child in `field`.
    fn field(&self, node: Node, field: &str) -> Option<&'s str> {
        Some(self.text(node.child_by_field_name(field)?)).filter(|text| !text.is_empty())
    }

    /// Visits each of `node`'s children, handing each Rust item the
    /// attributes just above it.
    fn children(&mut self, node: Node, scope: &Scope) {
        let mut attributes: Vec<&'s str> = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "attribute_item" => attributes.push(self.text(child)),
                "line_comment" | "block_comment" | "comment" => {}
                _ => {
                    self.visit(child, scope, &attributes);
                    attributes.clear();
                }
            }
        }
    }

    fn visit(&mut self, node: Node, scope: &Scope, attributes: &[&str]) {
        match self.lang {
            Lang::Rust => self.rust(node, scope, attributes),
            Lang::Go => self.go(node, scope),
            Lang::Python => self.python(node, scope),
            Lang::TypeScript | Lang::JavaScript => self.typescript(node, scope),
            Lang::Java => self.java(node, scope),
            Lang::C | Lang::Cpp => self.c(node, scope),
            _ => {}
        }
    }

    /// Adds a definition named `name`, spanning `node`, its name at
    /// `name_node`, and gives it back for what else it says.
    fn push(
        &mut self,
        scope: &Scope,
        name: &str,
        kind: DefKind,
        node: Node,
        name_node: Node,
    ) -> &mut Found {
        let mut segments = scope.path.clone();
        segments.push(name.to_string());
        let start = line_of(node.start_position().row);
        self.defs.push(Found {
            segments,
            kind,
            start,
            end: end_line(node).max(start),
            line: line_of(name_node.start_position().row),
            names: Vec::new(),
            ty: None,
            test: scope.test,
            decl: false,
        });
        self.defs.last_mut().expect("just pushed")
    }

    /// Adds the definition `node` makes, named by its child in `field`.
    fn named(
        &mut self,
        scope: &Scope,
        node: Node,
        field: &str,
        kind: DefKind,
    ) -> Option<&mut Found> {
        let name_node = node.child_by_field_name(field)?;
        let name = self.text(name_node);
        if name.is_empty() {
            return None;
        }
        Some(self.push(scope, name, kind, node, name_node))
    }

    /// A function, or a method among members.
    fn function_kind(scope: &Scope) -> DefKind {
        if scope.members {
            DefKind::Method
        } else {
            DefKind::Function
        }
    }

    // Rust

    fn rust(&mut self, node: Node, scope: &Scope, attributes: &[&str]) {
        let test = scope.test
            || attributes
                .iter()
                .any(|attribute| TEST_ATTRIBUTE.is_match(attribute));
        let scope = &Scope {
            test,
            ..scope.clone()
        };
        match node.kind() {
            "mod_item" => {
                let Some(found) = self.named(scope, node, "name", DefKind::Module) else {
                    return;
                };
                let name = found.segments.last().cloned().unwrap_or_default();
                let body = node.child_by_field_name("body");
                found.decl = body.is_none();
                if let Some(body) = body {
                    let mut inner = scope.under(&name);
                    inner.members = false;
                    inner.test |= name == "tests" || name == "test";
                    self.children(body, &inner);
                }
            }
            "struct_item" | "union_item" | "enum_item" | "trait_item" => {
                let kind = match node.kind() {
                    "struct_item" => DefKind::Struct,
                    "union_item" => DefKind::Union,
                    "enum_item" => DefKind::Enum,
                    _ => DefKind::Trait,
                };
                let Some(found) = self.named(scope, node, "name", kind) else {
                    return;
                };
                let name = found.segments.last().cloned().unwrap_or_default();
                if let Some(body) = node.child_by_field_name("body") {
                    let mut inner = scope.under(&name);
                    inner.members = kind == DefKind::Trait;
                    inner.clap = derives(attributes, &["Parser", "Args", "Subcommand"]);
                    inner.rename_all = attribute_value(attributes, "serde", "rename_all");
                    self.children(body, &inner);
                }
            }
            "impl_item" => {
                let (Some(ty), Some(body)) = (
                    node.child_by_field_name("type")
                        .and_then(|ty| self.rust_type_name(ty)),
                    node.child_by_field_name("body"),
                ) else {
                    return;
                };
                self.children(body, &scope.under(&ty));
            }
            "function_item" => {
                self.named(scope, node, "name", Self::function_kind(scope));
            }
            "function_signature_item" => {
                if let Some(found) = self.named(scope, node, "name", Self::function_kind(scope)) {
                    found.decl = true;
                }
            }
            "const_item" | "static_item" => {
                let kind = match node.kind() {
                    "const_item" => DefKind::Const,
                    _ => DefKind::Static,
                };
                let ty = self.field(node, "type").map(short);
                if let Some(found) = self.named(scope, node, "name", kind) {
                    found.ty = ty;
                }
            }
            "type_item" | "associated_type" => {
                self.named(scope, node, "name", DefKind::Type);
            }
            "macro_definition" => {
                self.named(scope, node, "name", DefKind::Macro);
            }
            "enum_variant" => {
                let body = node.child_by_field_name("body");
                let ty = body.map(|body| short(self.text(body)));
                let clap = scope.clap;
                let Some(found) = self.named(scope, node, "name", DefKind::Variant) else {
                    return;
                };
                found.ty = ty;
                let name = found.segments.last().cloned().unwrap_or_default();
                if clap {
                    found.names = subcommand_words(&name, attributes);
                }
                if let Some(body) = body.filter(|body| body.kind() == "field_declaration_list") {
                    let mut inner = scope.under(&name);
                    inner.clap = clap;
                    inner.rename_all = scope.rename_all.clone();
                    self.children(body, &inner);
                }
            }
            "field_declaration" => {
                let ty = self.field(node, "type").map(short);
                let rename_all = scope.rename_all.clone();
                let Some(found) = self.named(scope, node, "name", DefKind::Field) else {
                    return;
                };
                let name = found.segments.last().cloned().unwrap_or_default();
                found.ty = ty;
                found.names = field_names(&name, attributes, rename_all.as_deref());
            }
            "foreign_mod_item" => {
                if let Some(body) = node.child_by_field_name("body") {
                    self.children(body, scope);
                }
            }
            _ => {}
        }
    }

    /// The name of the type an `impl` is for: `Session` in `impl<T>
    /// Session<T>`, `&Session` or `crate::session::Session`.
    fn rust_type_name(&self, node: Node) -> Option<String> {
        match node.kind() {
            "type_identifier" | "primitive_type" => Some(self.text(node).to_string()),
            "generic_type" | "reference_type" | "pointer_type" => {
                self.rust_type_name(node.child_by_field_name("type")?)
            }
            "scoped_type_identifier" => Some(self.field(node, "name")?.to_string()),
            _ => None,
        }
    }

    // Go

    fn go(&mut self, node: Node, scope: &Scope) {
        match node.kind() {
            "package_clause" => {
                let mut cursor = node.walk();
                let name = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "package_identifier");
                if let Some(name) = name {
                    self.package = vec![self.text(name).to_string()];
                }
            }
            "function_declaration" => {
                self.named(scope, node, "name", DefKind::Function);
            }
            "method_declaration" => {
                let receiver = node.child_by_field_name("receiver").and_then(|receiver| {
                    let mut cursor = receiver.walk();
                    let parameter = receiver
                        .named_children(&mut cursor)
                        .find(|child| child.kind() == "parameter_declaration")?;
                    self.go_type_name(parameter.child_by_field_name("type")?)
                });
                let scope = match &receiver {
                    Some(receiver) => scope.under(receiver),
                    None => scope.clone(),
                };
                self.named(&scope, node, "name", DefKind::Method);
            }
            "type_declaration" => {
                let mut cursor = node.walk();
                for spec in node.named_children(&mut cursor) {
                    if matches!(spec.kind(), "type_spec" | "type_alias") {
                        self.go_type(spec, scope);
                    }
                }
            }
            "const_declaration" | "var_declaration" | "var_spec_list" => {
                let kind = match node.kind() {
                    "const_declaration" => DefKind::Const,
                    _ => DefKind::Variable,
                };
                let mut cursor = node.walk();
                for spec in node.named_children(&mut cursor) {
                    match spec.kind() {
                        "const_spec" | "var_spec" => {
                            self.each_named(scope, spec, kind);
                        }
                        "var_spec_list" => self.go(spec, scope),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Adds a definition for each name `node` declares in its `name`
    /// fields, as Go's `var a, b = 1, 2` and its struct fields do.
    fn each_named(&mut self, scope: &Scope, node: Node, kind: DefKind) -> Vec<usize> {
        let mut cursor = node.walk();
        let names: Vec<Node> = node.children_by_field_name("name", &mut cursor).collect();
        let mut added = Vec::new();
        for name in names {
            let text = self.text(name);
            if !text.is_empty() && text != "_" {
                self.push(scope, text, kind, node, name);
                added.push(self.defs.len() - 1);
            }
        }
        added
    }

    fn go_type(&mut self, spec: Node, scope: &Scope) {
        let ty = spec.child_by_field_name("type");
        let kind = match ty.map(|ty| ty.kind()) {
            Some("struct_type") => DefKind::Struct,
            Some("interface_type") => DefKind::Interface,
            _ => DefKind::Type,
        };
        let Some(found) = self.named(scope, spec, "name", kind) else {
            return;
        };
        let name = found.segments.last().cloned().unwrap_or_default();
        let Some(ty) = ty else { return };
        let inner = scope.under(&name);
        let mut cursor = ty.walk();
        for child in ty.named_children(&mut cursor) {
            match child.kind() {
                "field_declaration_list" => {
                    let mut cursor = child.walk();
                    for field in child.named_children(&mut cursor) {
                        if field.kind() != "field_declaration" {
                            continue;
                        }
                        let ty = self.field(field, "type").map(short);
                        let tag = self
                            .field(field, "tag")
                            .map(struct_tag_names)
                            .unwrap_or_default();
                        for at in self.each_named(&inner, field, DefKind::Field) {
                            let found = &mut self.defs[at];
                            found.ty = ty.clone();
                            found.names = tag.clone();
                            if let Some(name) = found.segments.last()
                                && !found.names.contains(name)
                            {
                                found.names.insert(0, name.clone());
                            }
                        }
                    }
                }
                "method_elem" | "method_spec" => {
                    if let Some(found) = self.named(&inner, child, "name", DefKind::Method) {
                        found.decl = true;
                    }
                }
                _ => {}
            }
        }
    }

    /// The name of a Go type a method's receiver is: `Server` in `*Server`
    /// or `Server[T]`.
    fn go_type_name(&self, node: Node) -> Option<String> {
        match node.kind() {
            "type_identifier" => Some(self.text(node).to_string()),
            "pointer_type" | "parenthesized_type" => {
                let mut cursor = node.walk();
                let inner = node.named_children(&mut cursor).next()?;
                self.go_type_name(inner)
            }
            "generic_type" => self.go_type_name(node.child_by_field_name("type")?),
            _ => None,
        }
    }

    // Python

    fn python(&mut self, node: Node, scope: &Scope) {
        match node.kind() {
            "class_definition" => {
                let Some(found) = self.named(scope, node, "name", DefKind::Class) else {
                    return;
                };
                let name = found.segments.last().cloned().unwrap_or_default();
                if let Some(body) = node.child_by_field_name("body") {
                    self.children(body, &scope.under(&name));
                }
            }
            "function_definition" => {
                self.named(scope, node, "name", Self::function_kind(scope));
            }
            "decorated_definition" => {
                if let Some(definition) = node.child_by_field_name("definition") {
                    self.python(definition, scope);
                }
            }
            "expression_statement" => {
                let mut cursor = node.walk();
                let Some(assignment) = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "assignment")
                else {
                    return;
                };
                let Some(left) = assignment
                    .child_by_field_name("left")
                    .filter(|left| left.kind() == "identifier")
                else {
                    return;
                };
                let name = self.text(left);
                let kind = if scope.members {
                    DefKind::Field
                } else if is_constant(name) {
                    DefKind::Const
                } else {
                    DefKind::Variable
                };
                let ty = self.field(assignment, "type").map(short);
                let found = self.push(scope, name, kind, node, left);
                found.ty = ty;
            }
            _ => {}
        }
    }

    // TypeScript and JavaScript

    fn typescript(&mut self, node: Node, scope: &Scope) {
        match node.kind() {
            "export_statement" => {
                if let Some(declaration) = node.child_by_field_name("declaration") {
                    self.typescript(declaration, scope);
                }
            }
            "ambient_declaration" => self.children(node, scope),
            "expression_statement" => {
                let mut cursor = node.walk();
                let module = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "internal_module");
                if let Some(module) = module {
                    self.typescript(module, scope);
                }
            }
            "class_declaration" | "abstract_class_declaration" | "interface_declaration" => {
                let kind = match node.kind() {
                    "interface_declaration" => DefKind::Interface,
                    _ => DefKind::Class,
                };
                let Some(found) = self.named(scope, node, "name", kind) else {
                    return;
                };
                let name = found.segments.last().cloned().unwrap_or_default();
                if let Some(body) = node.child_by_field_name("body") {
                    self.children(body, &scope.under(&name));
                }
            }
            "type_alias_declaration" => {
                self.named(scope, node, "name", DefKind::Type);
            }
            "enum_declaration" => {
                let Some(found) = self.named(scope, node, "name", DefKind::Enum) else {
                    return;
                };
                let name = found.segments.last().cloned().unwrap_or_default();
                let Some(body) = node.child_by_field_name("body") else {
                    return;
                };
                let inner = scope.under(&name);
                let mut cursor = body.walk();
                for member in body.named_children(&mut cursor) {
                    match member.kind() {
                        "property_identifier" => {
                            self.push(&inner, self.text(member), DefKind::Variant, member, member);
                        }
                        "enum_assignment" => {
                            self.named(&inner, member, "name", DefKind::Variant);
                        }
                        _ => {}
                    }
                }
            }
            "function_declaration" | "generator_function_declaration" => {
                self.named(scope, node, "name", DefKind::Function);
            }
            "function_signature" => {
                if let Some(found) = self.named(scope, node, "name", DefKind::Function) {
                    found.decl = true;
                }
            }
            "method_definition" => {
                self.named(scope, node, "name", DefKind::Method);
            }
            "method_signature" | "abstract_method_signature" => {
                if let Some(found) = self.named(scope, node, "name", DefKind::Method) {
                    found.decl = true;
                }
            }
            "public_field_definition" | "property_signature" => {
                let ty = node
                    .child_by_field_name("type")
                    .map(|ty| short(self.text(ty).trim_start_matches(':').trim()));
                if let Some(found) = self.named(scope, node, "name", DefKind::Field) {
                    found.ty = ty;
                }
            }
            "lexical_declaration" | "variable_declaration" if !scope.members => {
                let constant = self.text(node).starts_with("const");
                let mut cursor = node.walk();
                for declarator in node.named_children(&mut cursor) {
                    if declarator.kind() != "variable_declarator" {
                        continue;
                    }
                    let Some(name) = declarator
                        .child_by_field_name("name")
                        .filter(|name| name.kind() == "identifier")
                    else {
                        continue;
                    };
                    let kind = match declarator
                        .child_by_field_name("value")
                        .map(|value| value.kind())
                    {
                        Some(
                            "arrow_function"
                            | "function_expression"
                            | "function"
                            | "generator_function",
                        ) => DefKind::Function,
                        Some("class") => DefKind::Class,
                        _ if constant => DefKind::Const,
                        _ => DefKind::Variable,
                    };
                    let text = self.text(name);
                    self.push(scope, text, kind, node, name);
                }
            }
            "internal_module" | "module" => {
                let Some(name_node) = node.child_by_field_name("name") else {
                    return;
                };
                let name = self
                    .text(name_node)
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_string();
                self.push(scope, &name, DefKind::Module, node, name_node);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut inner = scope.under(&name);
                    inner.members = false;
                    self.children(body, &inner);
                }
            }
            _ => {}
        }
    }

    // Java

    fn java(&mut self, node: Node, scope: &Scope) {
        match node.kind() {
            "package_declaration" => {
                let mut cursor = node.walk();
                let name = node
                    .named_children(&mut cursor)
                    .find(|child| matches!(child.kind(), "scoped_identifier" | "identifier"));
                if let Some(name) = name {
                    self.package = self.text(name).split('.').map(String::from).collect();
                }
            }
            "class_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "annotation_type_declaration"
            | "enum_declaration" => {
                let kind = match node.kind() {
                    "interface_declaration" | "annotation_type_declaration" => DefKind::Interface,
                    "enum_declaration" => DefKind::Enum,
                    _ => DefKind::Class,
                };
                let Some(found) = self.named(scope, node, "name", kind) else {
                    return;
                };
                let name = found.segments.last().cloned().unwrap_or_default();
                if let Some(body) = node.child_by_field_name("body") {
                    self.children(body, &scope.under(&name));
                }
            }
            "enum_body_declarations" => self.children(node, scope),
            "enum_constant" => {
                self.named(scope, node, "name", DefKind::Variant);
            }
            "method_declaration" => {
                let body = node.child_by_field_name("body");
                if let Some(found) = self.named(scope, node, "name", DefKind::Method) {
                    found.decl = body.is_none();
                }
            }
            "field_declaration" | "constant_declaration" => {
                let ty = self.field(node, "type").map(short);
                let mut cursor = node.walk();
                let declarators: Vec<Node> = node
                    .children_by_field_name("declarator", &mut cursor)
                    .collect();
                for declarator in declarators {
                    if let Some(found) = self.named(scope, declarator, "name", DefKind::Field) {
                        found.ty = ty.clone();
                        found.start = line_of(node.start_position().row);
                        found.end = end_line(node).max(found.start);
                    }
                }
            }
            _ => {}
        }
    }

    // C and C++

    fn c(&mut self, node: Node, scope: &Scope) {
        match node.kind() {
            "function_definition" => {
                let Some(declarator) = node.child_by_field_name("declarator") else {
                    return;
                };
                let Some(named) = self.declared(declarator) else {
                    return;
                };
                if !named.function || named.special(scope) {
                    return;
                }
                let scope = named.scope_in(scope);
                let kind = match scope.members || !named.scopes.is_empty() {
                    true => DefKind::Method,
                    false => DefKind::Function,
                };
                self.push(&scope, &named.name, kind, node, named.node);
            }
            "declaration" | "field_declaration" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    self.c(ty, scope);
                }
                let mut cursor = node.walk();
                let declarators: Vec<Node> = node
                    .children_by_field_name("declarator", &mut cursor)
                    .collect();
                for declarator in declarators {
                    let Some(named) = self.declared(declarator) else {
                        continue;
                    };
                    if named.special(scope) {
                        continue;
                    }
                    let kind = match (named.function, scope.members) {
                        (true, true) => DefKind::Method,
                        (true, false) => DefKind::Function,
                        (false, true) => DefKind::Field,
                        (false, false) => DefKind::Variable,
                    };
                    let ty = self.field(node, "type").map(short);
                    let scope = named.scope_in(scope);
                    let found = self.push(&scope, &named.name, kind, node, named.node);
                    found.decl = named.function;
                    found.ty = ty.filter(|_| !named.function);
                }
            }
            "struct_specifier" | "union_specifier" | "class_specifier" | "enum_specifier" => {
                let (Some(name_node), Some(body)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("body"),
                ) else {
                    return;
                };
                let kind = match node.kind() {
                    "struct_specifier" => DefKind::Struct,
                    "union_specifier" => DefKind::Union,
                    "class_specifier" => DefKind::Class,
                    _ => DefKind::Enum,
                };
                let name = self.text(name_node).to_string();
                self.push(scope, &name, kind, node, name_node);
                self.c_body(body, &scope.under(&name));
            }
            "type_definition" => {
                let ty = node.child_by_field_name("type");
                let mut cursor = node.walk();
                let declarators: Vec<Node> = node
                    .children_by_field_name("declarator", &mut cursor)
                    .collect();
                let names: Vec<(String, Node)> = declarators
                    .iter()
                    .filter_map(|&declarator| self.declared(declarator))
                    .map(|named| (named.name, named.node))
                    .collect();
                for (name, name_node) in &names {
                    self.push(scope, name, DefKind::Type, node, *name_node);
                }
                // A typedef of a struct with no name of its own names its
                // fields under the typedef's.
                if let Some(ty) = ty {
                    match (
                        ty.child_by_field_name("name"),
                        ty.child_by_field_name("body"),
                        names.first(),
                    ) {
                        (Some(_), _, _) => self.c(ty, scope),
                        (None, Some(body), Some((name, _))) => {
                            self.c_body(body, &scope.under(name))
                        }
                        _ => {}
                    }
                }
            }
            "alias_declaration" => {
                self.named(scope, node, "name", DefKind::Type);
            }
            "namespace_definition" => {
                let Some(name_node) = node.child_by_field_name("name") else {
                    if let Some(body) = node.child_by_field_name("body") {
                        self.children(body, scope);
                    }
                    return;
                };
                let name = self.text(name_node).to_string();
                self.push(scope, &name, DefKind::Module, node, name_node);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut inner = scope.clone();
                    inner.path.extend(name.split("::").map(String::from));
                    self.children(body, &inner);
                }
            }
            "preproc_def" | "preproc_function_def" => {
                self.named(scope, node, "name", DefKind::Macro);
            }
            "template_declaration"
            | "preproc_ifdef"
            | "preproc_if"
            | "preproc_else"
            | "preproc_elif"
            | "preproc_elifdef"
            | "declaration_list" => self.children(node, scope),
            "linkage_specification" => {
                if let Some(body) = node.child_by_field_name("body") {
                    match body.kind() {
                        "declaration_list" => self.children(body, scope),
                        _ => self.c(body, scope),
                    }
                }
            }
            _ => {}
        }
    }

    /// The members of a struct's, a class's or an enum's body.
    fn c_body(&mut self, body: Node, scope: &Scope) {
        if body.kind() == "enumerator_list" {
            let mut cursor = body.walk();
            for enumerator in body.named_children(&mut cursor) {
                if enumerator.kind() == "enumerator" {
                    self.named(scope, enumerator, "name", DefKind::Variant);
                }
            }
        } else {
            self.children(body, scope);
        }
    }

    /// What a C declarator declares: its name, the scopes written before it
    /// (`Server::` in `Server::serve`), and whether it's a function.
    fn declared<'t>(&self, declarator: Node<'t>) -> Option<Declared<'t>> {
        let mut node = declarator;
        let mut function = false;
        loop {
            match node.kind() {
                "function_declarator" => {
                    function = true;
                    node = node.child_by_field_name("declarator")?;
                }
                "pointer_declarator"
                | "reference_declarator"
                | "array_declarator"
                | "init_declarator"
                | "parenthesized_declarator"
                | "attributed_declarator" => {
                    node = match node.child_by_field_name("declarator") {
                        Some(inner) => inner,
                        None => {
                            let mut cursor = node.walk();
                            node.named_children(&mut cursor).last()?
                        }
                    };
                }
                "identifier" | "field_identifier" | "type_identifier" | "destructor_name"
                | "operator_name" => {
                    return Some(Declared {
                        name: self.text(node).to_string(),
                        scopes: Vec::new(),
                        function,
                        node,
                    });
                }
                "qualified_identifier" => {
                    let text = self.text(node);
                    let mut parts: Vec<String> = text
                        .split("::")
                        .map(|part| part.trim().to_string())
                        .collect();
                    let name = parts.pop().filter(|name| !name.is_empty())?;
                    parts.retain(|part| !part.is_empty());
                    let parts = parts
                        .into_iter()
                        .map(|part| part.split('<').next().unwrap_or(&part).to_string());
                    return Some(Declared {
                        name,
                        scopes: parts.collect(),
                        function,
                        node,
                    });
                }
                _ => return None,
            }
        }
    }

    /// Flags and subcommands a command line's library defines in calls,
    /// anywhere in the file: cobra's `Use:` and `Flags().StringVarP(&x,
    /// "file", "f", ...)`, the flag package's, Python's `add_argument` and
    /// click's `option`, commander's `.option("-p, --port <n>")` and
    /// `.command("build")`.
    fn calls<'t>(&mut self, root: Node<'t>) {
        if !matches!(
            self.lang,
            Lang::Go | Lang::Python | Lang::TypeScript | Lang::JavaScript
        ) {
            return;
        }
        let scope = Scope::default();
        let mut cursor = root.walk();
        let mut found: Vec<(String, DefKind, Vec<String>, Node<'t>)> = Vec::new();
        'walk: loop {
            let node = cursor.node();
            match (self.lang, node.kind()) {
                (Lang::Go, "composite_literal") => {
                    let ty = self.field(node, "type").unwrap_or("");
                    if ty.ends_with("Command")
                        && let Some(word) = self.go_command_word(node)
                    {
                        found.push((word.clone(), DefKind::Command, vec![word], node));
                    }
                }
                (Lang::Go, "call_expression") => {
                    if let Some((name, names)) = self.go_flag(node) {
                        found.push((name, DefKind::Flag, names, node));
                    }
                }
                (Lang::Python, "call")
                | (Lang::TypeScript | Lang::JavaScript, "call_expression") => {
                    if let Some((name, kind, names)) = self.call_flag(node) {
                        found.push((name, kind, names, node));
                    }
                }
                _ => {}
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    break 'walk;
                }
            }
        }
        for (name, kind, names, node) in found {
            let def = self.push(&scope, &name, kind, node, node);
            def.names = names;
        }
    }

    /// The word a cobra command's `Use:` starts with.
    fn go_command_word(&self, literal: Node) -> Option<String> {
        let body = literal.child_by_field_name("body")?;
        let mut cursor = body.walk();
        for element in body.named_children(&mut cursor) {
            if element.kind() != "keyed_element" {
                continue;
            }
            let (Some(key), Some(value)) = (element.named_child(0), element.named_child(1)) else {
                continue;
            };
            if self.text(key) == "Use" {
                let usage = unquote(self.text(value));
                let word = usage.split_whitespace().next()?;
                return Some(word.to_string()).filter(|word| is_word(word));
            }
        }
        None
    }

    /// The flag a Go call defines: `fs.StringVarP(&x, "file", "f", ...)`
    /// on a flag set, its name and the names it goes by (`--file`, `-f`).
    fn go_flag(&self, call: Node) -> Option<(String, Vec<String>)> {
        let function = call.child_by_field_name("function")?;
        if function.kind() != "selector_expression" {
            return None;
        }
        let method = self.field(function, "field")?;
        let operand = self.field(function, "operand")?;
        if !GO_FLAG_METHOD.is_match(method)
            || !(operand.to_lowercase().contains("flag") || operand == "fs")
        {
            return None;
        }
        let arguments = call.child_by_field_name("arguments")?;
        let mut cursor = arguments.walk();
        let strings: Vec<String> = arguments
            .named_children(&mut cursor)
            .filter(|argument| {
                matches!(
                    argument.kind(),
                    "interpreted_string_literal" | "raw_string_literal"
                )
            })
            .map(|argument| unquote(self.text(argument)).to_string())
            .collect();
        let name = strings.first().filter(|name| is_word(name))?;
        let mut names = vec![format!("--{name}")];
        if method.ends_with('P')
            && let Some(short) = strings.get(1).filter(|short| short.chars().count() == 1)
        {
            names.push(format!("-{short}"));
        }
        Some((format!("--{name}"), names))
    }

    /// The flag or subcommand a Python or JavaScript call defines:
    /// `add_argument("--verbose", "-v")`, `option("-p, --port <n>")`,
    /// `.command("build <dir>")`.
    fn call_flag(&self, call: Node) -> Option<(String, DefKind, Vec<String>)> {
        let function = call.child_by_field_name("function")?;
        let method = match function.kind() {
            "attribute" => self.field(function, "attribute")?,
            "member_expression" => self.field(function, "property")?,
            "identifier" => self.text(function),
            _ => return None,
        };
        let arguments = call.child_by_field_name("arguments")?;
        let mut cursor = arguments.walk();
        let strings: Vec<&str> = arguments
            .named_children(&mut cursor)
            .filter(|argument| argument.kind() == "string")
            .map(|argument| unquote(self.text(argument)))
            .collect();
        match method {
            "add_argument" | "add_option" | "option" | "Option" | "requiredOption"
            | "addOption" => {
                let names: Vec<String> = strings
                    .iter()
                    .flat_map(|text| text.split([',', ' ', '|']))
                    .map(|word| word.trim())
                    .filter(|word| {
                        word.starts_with('-')
                            && word.len() > 1
                            && is_word(word.trim_start_matches('-'))
                    })
                    .map(String::from)
                    .collect();
                let name = names
                    .iter()
                    .find(|name| name.starts_with("--"))
                    .or(names.first())?
                    .clone();
                Some((name, DefKind::Flag, names))
            }
            "command" if matches!(self.lang, Lang::TypeScript | Lang::JavaScript) => {
                let word = strings.first()?.split_whitespace().next()?;
                Some(word.to_string())
                    .filter(|word| is_word(word))
                    .map(|word| (word.clone(), DefKind::Command, vec![word]))
            }
            _ => None,
        }
    }
}

/// What a C declarator declares.
struct Declared<'s> {
    name: String,
    scopes: Vec<String>,
    function: bool,
    node: Node<'s>,
}

impl Declared<'_> {
    /// Whether it's a constructor or a destructor, which prose names by
    /// their class.
    fn special(&self, scope: &Scope) -> bool {
        let class = self.scopes.last().or(scope.path.last());
        self.name.starts_with('~') || class.is_some_and(|class| *class == self.name)
    }

    /// The scope it's defined in: `scope`, with the scopes written before
    /// its name.
    fn scope_in(&self, scope: &Scope) -> Scope {
        let mut inner = scope.clone();
        inner.path.extend(self.scopes.iter().cloned());
        inner.members |= !self.scopes.is_empty();
        inner
    }
}

/// A line from tree-sitter's row, which counts from 0.
fn line_of(row: usize) -> u32 {
    u32::try_from(row + 1).unwrap_or(u32::MAX)
}

/// The last line `node` is on: the line before its end when it ends at the
/// start of one, as a C macro's does with its newline.
fn end_line(node: Node) -> u32 {
    let (start, end) = (node.start_position(), node.end_position());
    if end.column == 0 && end.row > start.row {
        line_of(end.row - 1)
    } else {
        line_of(end.row)
    }
}

/// Text cut to what a type needs to be followed by, on one line.
fn short(text: &str) -> String {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(240) {
        Some((at, _)) => text[..at].to_string(),
        None => text,
    }
}

/// A string literal's text without its quotes.
fn unquote(text: &str) -> &str {
    let text = text.trim();
    let text = text
        .strip_prefix(['r', 'b', 'f', 'u'])
        .filter(|rest| rest.starts_with(['"', '\'']))
        .unwrap_or(text);
    text.trim_matches(|c| c == '"' || c == '\'' || c == '`')
}

/// Whether `text` is a name a command line could take: letters, digits,
/// `-` and `_`, starting with a letter.
fn is_word(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Whether a name is written as a constant's: `MAX_SIZE`.
fn is_constant(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_uppercase()) && !name.chars().any(|c| c.is_ascii_lowercase())
}

/// A Rust attribute that makes what's under it a test's.
static TEST_ATTRIBUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^#\[\s*(?:\w+::)*test\b|cfg\(\s*test\s*\)").expect("a valid regex")
});

/// The methods of Go's flag and pflag packages that define a flag.
static GO_FLAG_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:Var|String|Bool|Int|Int8|Int16|Int32|Int64|Uint|Uint8|Uint16|Uint32|Uint64|Float32|Float64|Duration|StringSlice|StringArray|StringToString|IntSlice|UintSlice|Count|IP|IPNet|BytesHex|BytesBase64|Func|BoolFunc|TextVar)(?:Var)?P?$",
    )
    .expect("a valid regex")
});

/// Whether one of `attributes` derives one of `traits`.
fn derives(attributes: &[&str], traits: &[&str]) -> bool {
    static DERIVE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"derive\(([^)]*)\)").expect("a valid regex"));
    attributes.iter().any(|attribute| {
        DERIVE.captures_iter(attribute).any(|derived| {
            derived[1]
                .split(',')
                .map(|name| name.trim().rsplit("::").next().unwrap_or(""))
                .any(|name| traits.contains(&name))
        })
    })
}

/// The attributes among `attributes` that are `#[<tool>(...)]`, their
/// insides.
fn tool_attributes<'a>(
    attributes: &'a [&'a str],
    tools: &'a [&'a str],
) -> impl Iterator<Item = &'a str> + 'a {
    attributes.iter().filter_map(move |attribute| {
        let inside = attribute
            .trim()
            .strip_prefix("#[")?
            .trim_end_matches(']')
            .trim();
        tools.iter().find_map(|tool| {
            inside
                .strip_prefix(tool)?
                .trim_start()
                .strip_prefix('(')?
                .strip_suffix(')')
        })
    })
}

/// The value of `key = "value"` in a `#[<tool>(...)]` attribute.
fn attribute_value(attributes: &[&str], tool: &str, key: &str) -> Option<String> {
    attribute_values(attributes, &[tool], key)
        .into_iter()
        .next()
}

/// Every value of `key = "value"` in the `#[<tool>(...)]` attributes.
fn attribute_values(attributes: &[&str], tools: &[&str], key: &str) -> Vec<String> {
    let pattern = Regex::new(&format!(
        r#"(?:^|[\s,(]){}\s*=\s*"([^"]*)""#,
        regex::escape(key)
    ))
    .expect("a valid regex");
    tool_attributes(attributes, tools)
        .flat_map(|inside| {
            pattern
                .captures_iter(inside)
                .map(|value| value[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Whether a `#[<tool>(...)]` attribute says `word` on its own or as a
/// key: `long` in `#[arg(long, short)]` and `#[arg(long = "x")]`.
fn attribute_says(attributes: &[&str], tools: &[&str], word: &str) -> bool {
    let pattern =
        Regex::new(&format!(r"(?:^|[\s,(]){}\b", regex::escape(word))).expect("a valid regex");
    tool_attributes(attributes, tools).any(|inside| pattern.is_match(inside))
}

/// The words a clap subcommand goes by: its `name`, else its variant's name
/// in kebab case, and its aliases.
fn subcommand_words(variant: &str, attributes: &[&str]) -> Vec<String> {
    const CLAP: &[&str] = &["command", "clap", "structopt"];
    let mut words = attribute_values(attributes, CLAP, "name");
    if words.is_empty() {
        words.push(kebab(variant));
    }
    words.extend(attribute_values(attributes, CLAP, "alias"));
    words.extend(attribute_values(attributes, CLAP, "visible_alias"));
    words
}

/// The names a Rust field goes by beside its own: its key as serde reads
/// it, and its flags as clap reads it.
fn field_names(field: &str, attributes: &[&str], rename_all: Option<&str>) -> Vec<String> {
    let mut names = attribute_values(attributes, &["serde"], "rename");
    if names.is_empty() {
        names.push(match rename_all {
            Some(case) => renamed(field, case),
            None => field.to_string(),
        });
    }
    names.extend(attribute_values(attributes, &["serde"], "alias"));
    const CLAP: &[&str] = &["arg", "clap", "structopt"];
    if attribute_says(attributes, CLAP, "long") {
        let long = attribute_values(attributes, CLAP, "long");
        match long.is_empty() {
            true => names.push(format!("--{}", kebab(field))),
            false => names.extend(long.iter().map(|long| format!("--{long}"))),
        }
        names.extend(
            attribute_values(attributes, CLAP, "alias")
                .iter()
                .map(|alias| format!("--{alias}")),
        );
        names.extend(
            attribute_values(attributes, CLAP, "visible_alias")
                .iter()
                .map(|alias| format!("--{alias}")),
        );
    }
    static SHORT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:^|[\s,(])short\s*=\s*'(.)'").expect("a valid regex"));
    let mut short = tool_attributes(attributes, CLAP)
        .find_map(|inside| SHORT.captures(inside).map(|short| short[1].to_string()));
    if short.is_none() && attribute_says(attributes, CLAP, "short") {
        short = field.chars().next().map(String::from);
    }
    names.extend(short.map(|short| format!("-{short}")));
    names
}

/// A snake_case name as serde's `rename_all` writes it.
fn renamed(field: &str, case: &str) -> String {
    let words: Vec<&str> = field.split('_').filter(|word| !word.is_empty()).collect();
    let capitalized = |word: &&str| {
        let mut chars = word.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect::<String>())
            .unwrap_or_default()
    };
    match case {
        "lowercase" => field.replace('_', "").to_lowercase(),
        "UPPERCASE" => field.replace('_', "").to_uppercase(),
        "PascalCase" => words.iter().map(capitalized).collect(),
        "camelCase" => {
            let mut words = words.iter();
            let first = words
                .next()
                .map(|word| word.to_string())
                .unwrap_or_default();
            first + &words.map(capitalized).collect::<String>()
        }
        "SCREAMING_SNAKE_CASE" => field.to_uppercase(),
        "kebab-case" => field.replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => field.replace('_', "-").to_uppercase(),
        _ => field.to_string(),
    }
}

/// A Rust name in kebab case, as clap names subcommands and flags:
/// `RestartServer` and `restart_server` are `restart-server`.
pub fn kebab(name: &str) -> String {
    let mut kebab = String::new();
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if c == '_' {
            kebab.push('-');
        } else if c.is_uppercase() {
            if previous.is_some_and(|previous| previous.is_lowercase() || previous.is_ascii_digit())
            {
                kebab.push('-');
            }
            kebab.extend(c.to_lowercase());
        } else {
            kebab.push(c);
        }
        previous = Some(c);
    }
    kebab
}

/// The names a Go struct tag gives a field: `name` in `` `toml:"name"
/// json:"name,omitempty"` ``.
fn struct_tag_names(tag: &str) -> Vec<String> {
    static TAG: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?:toml|yaml|json|mapstructure|koanf|env)\s*:\s*"([^",]*)"#)
            .expect("a valid regex")
    });
    let mut names: Vec<String> = Vec::new();
    for name in TAG.captures_iter(tag) {
        let name = name[1].to_string();
        if !name.is_empty() && name != "-" && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests;
