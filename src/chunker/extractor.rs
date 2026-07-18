#![allow(dead_code)]

use super::ChunkKind;
use crate::file::Language;
use tree_sitter::Node;

/// Language-specific code extraction logic
///
/// Each language has different AST node types and conventions for:
/// - Finding definitions (functions, classes, etc.)
/// - Extracting names
/// - Building signatures
/// - Finding docstrings
///
/// This trait allows us to handle multiple languages with proper semantics.
pub trait LanguageExtractor: Send + Sync {
    /// Get the AST node types that represent definitions in this language
    ///
    /// For example:
    /// - Rust: `["function_item", "struct_item", "impl_item", ...]`
    /// - Python: `["function_definition", "class_definition"]`
    fn definition_types(&self) -> &[&'static str];

    /// Extract the name from a definition node
    ///
    /// Returns None if the node has no name (anonymous)
    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String>;

    /// Extract a function/method signature
    ///
    /// Examples:
    /// - Rust: `fn sort<T: Ord>(items: Vec<T>) -> Vec<T>`
    /// - Python: `def process(data: List[str]) -> Dict[str, int]`
    /// - TypeScript: `function compute(x: number): string`
    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String>;

    /// Extract docstring/documentation comments
    ///
    /// Different languages have different conventions:
    /// - Rust: `/// ` and `/** */`
    /// - Python: First string literal in function/class body
    /// - JavaScript/TypeScript: JSDoc `/** */`
    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String>;

    /// Classify a node into a ChunkKind
    fn classify(&self, node: Node) -> ChunkKind;

    /// Check if a node is a definition
    #[allow(dead_code)]
    fn is_definition(&self, node: Node) -> bool {
        self.definition_types().contains(&node.kind())
    }

    /// Build a label for a node (e.g., "Function: foo", "Class: Bar")
    fn build_label(&self, node: Node, source: &[u8]) -> Option<String> {
        let name = self.extract_name(node, source)?;
        let kind = self.classify(node);

        Some(match kind {
            ChunkKind::Function => format!("Function: {}", name),
            ChunkKind::Method => format!("Method: {}", name),
            ChunkKind::Class => format!("Class: {}", name),
            ChunkKind::Struct => format!("Struct: {}", name),
            ChunkKind::Enum => format!("Enum: {}", name),
            ChunkKind::Trait => format!("Trait: {}", name),
            ChunkKind::Interface => format!("Interface: {}", name),
            ChunkKind::Impl => format!("Impl: {}", name),
            ChunkKind::Mod => format!("Module: {}", name),
            ChunkKind::TypeAlias => format!("Type: {}", name),
            ChunkKind::Const => format!("Const: {}", name),
            ChunkKind::Static => format!("Static: {}", name),
            ChunkKind::Imports => format!("Imports: {}", name),
            ChunkKind::ModuleDocs => format!("ModuleDocs: {}", name),
            ChunkKind::Comment => format!("Comment: {}", name),
            _ => format!("Symbol: {}", name),
        })
    }
}

/// Get the appropriate extractor for a language
pub fn get_extractor(language: Language) -> Option<Box<dyn LanguageExtractor>> {
    match language {
        Language::Rust => Some(Box::new(RustExtractor)),
        Language::Python => Some(Box::new(PythonExtractor)),
        Language::JavaScript | Language::TypeScript => Some(Box::new(TypeScriptExtractor)),
        Language::C => Some(Box::new(CExtractor)),
        Language::Cpp => Some(Box::new(CppExtractor)),
        Language::CSharp => Some(Box::new(CSharpExtractor)),
        Language::Go => Some(Box::new(GoExtractor)),
        Language::Java => Some(Box::new(JavaExtractor)),
        Language::Dart => Some(Box::new(DartExtractor)),
        Language::Haxe => Some(Box::new(HaxeExtractor)),
        _ => None,
    }
}

/// Rust language extractor
pub struct RustExtractor;

impl LanguageExtractor for RustExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &[
            "function_item",
            "struct_item",
            "enum_item",
            "impl_item",
            "trait_item",
            "type_item",
            "mod_item",
            "const_item",
            "static_item",
        ]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        // Rust has consistent "name" field for most definitions
        node.child_by_field_name("name")?
            .utf8_text(source)
            .ok()
            .map(String::from)
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_item" => {
                // Build: fn name<T>(params) -> Return
                let mut sig = String::from("fn ");

                // Add name
                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name.utf8_text(source) {
                        sig.push_str(name_text);
                    }
                }

                // Add type parameters
                if let Some(type_params) = node.child_by_field_name("type_parameters") {
                    if let Ok(params_text) = type_params.utf8_text(source) {
                        sig.push_str(params_text);
                    }
                }

                // Add parameters
                if let Some(params) = node.child_by_field_name("parameters") {
                    if let Ok(params_text) = params.utf8_text(source) {
                        sig.push_str(params_text);
                    }
                }

                // Add return type
                if let Some(return_type) = node.child_by_field_name("return_type") {
                    if let Ok(ret_text) = return_type.utf8_text(source) {
                        sig.push_str(" -> ");
                        sig.push_str(ret_text);
                    }
                }

                Some(sig)
            }
            "struct_item" => {
                // Build: struct Name<T>
                let mut sig = String::from("struct ");

                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name.utf8_text(source) {
                        sig.push_str(name_text);
                    }
                }

                if let Some(type_params) = node.child_by_field_name("type_parameters") {
                    if let Ok(params_text) = type_params.utf8_text(source) {
                        sig.push_str(params_text);
                    }
                }

                Some(sig)
            }
            "enum_item" => {
                // Build: enum Name<T>
                let mut sig = String::from("enum ");

                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name.utf8_text(source) {
                        sig.push_str(name_text);
                    }
                }

                if let Some(type_params) = node.child_by_field_name("type_parameters") {
                    if let Ok(params_text) = type_params.utf8_text(source) {
                        sig.push_str(params_text);
                    }
                }

                Some(sig)
            }
            "trait_item" => {
                // Build: trait Name<T>
                let mut sig = String::from("trait ");

                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name.utf8_text(source) {
                        sig.push_str(name_text);
                    }
                }

                if let Some(type_params) = node.child_by_field_name("type_parameters") {
                    if let Ok(params_text) = type_params.utf8_text(source) {
                        sig.push_str(params_text);
                    }
                }

                Some(sig)
            }
            "impl_item" => {
                // Build: impl<T> Trait for Type
                let mut sig = String::from("impl");

                if let Some(type_params) = node.child_by_field_name("type_parameters") {
                    if let Ok(params_text) = type_params.utf8_text(source) {
                        sig.push_str(params_text);
                    }
                }

                if let Some(trait_name) = node.child_by_field_name("trait") {
                    if let Ok(trait_text) = trait_name.utf8_text(source) {
                        sig.push(' ');
                        sig.push_str(trait_text);
                        sig.push_str(" for");
                    }
                }

                if let Some(type_name) = node.child_by_field_name("type") {
                    if let Ok(type_text) = type_name.utf8_text(source) {
                        sig.push(' ');
                        sig.push_str(type_text);
                    }
                }

                Some(sig)
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        // Look for line_comment or block_comment nodes immediately before this node
        // Tree-sitter includes them as named siblings in some grammars

        // For now, we'll look at the previous siblings
        let parent = node.parent()?;
        let node_index = (0..parent.named_child_count())
            .find(|&i| parent.named_child(i as u32).map(|c| c.id()) == Some(node.id()))?;

        if node_index > 0 {
            if let Some(prev) = parent.named_child((node_index - 1) as u32) {
                if prev.kind() == "line_comment" || prev.kind() == "block_comment" {
                    if let Ok(text) = prev.utf8_text(source) {
                        // Check if it's a doc comment (/// or /**)
                        if text.trim_start().starts_with("///")
                            || text.trim_start().starts_with("/**")
                        {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }

        None
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "function_item" => {
                // Check if it's a method (inside impl block)
                if let Some(parent) = node.parent() {
                    if parent.kind() == "declaration_list" {
                        if let Some(grandparent) = parent.parent() {
                            if grandparent.kind() == "impl_item" {
                                return ChunkKind::Method;
                            }
                        }
                    }
                }
                ChunkKind::Function
            }
            "struct_item" => ChunkKind::Struct,
            "enum_item" => ChunkKind::Enum,
            "impl_item" => ChunkKind::Impl,
            "trait_item" => ChunkKind::Trait,
            "type_item" => ChunkKind::TypeAlias,
            "mod_item" => ChunkKind::Mod,
            "const_item" => ChunkKind::Const,
            "static_item" => ChunkKind::Static,
            _ => ChunkKind::Other,
        }
    }
}

/// Python language extractor
pub struct PythonExtractor;

impl LanguageExtractor for PythonExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &["function_definition", "class_definition"]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        node.child_by_field_name("name")?
            .utf8_text(source)
            .ok()
            .map(String::from)
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_definition" => {
                // Build: def name(params) -> Return:
                let mut sig = String::from("def ");

                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name.utf8_text(source) {
                        sig.push_str(name_text);
                    }
                }

                if let Some(params) = node.child_by_field_name("parameters") {
                    if let Ok(params_text) = params.utf8_text(source) {
                        sig.push_str(params_text);
                    }
                }

                if let Some(return_type) = node.child_by_field_name("return_type") {
                    if let Ok(ret_text) = return_type.utf8_text(source) {
                        sig.push_str(" -> ");
                        sig.push_str(ret_text);
                    }
                }

                Some(sig)
            }
            "class_definition" => {
                // Build: class Name(Base):
                let mut sig = String::from("class ");

                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name.utf8_text(source) {
                        sig.push_str(name_text);
                    }
                }

                if let Some(superclasses) = node.child_by_field_name("superclasses") {
                    if let Ok(bases_text) = superclasses.utf8_text(source) {
                        sig.push_str(bases_text);
                    }
                }

                Some(sig)
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        // Python docstrings are the first statement in the body if it's a string
        let body = node.child_by_field_name("body")?;

        let mut cursor = body.walk();
        // Only check first statement
        if let Some(child) = body.named_children(&mut cursor).next() {
            if child.kind() == "expression_statement" {
                // Check if it contains a string
                let mut expr_cursor = child.walk();
                for expr_child in child.named_children(&mut expr_cursor) {
                    if expr_child.kind() == "string" {
                        return expr_child.utf8_text(source).ok().map(String::from);
                    }
                }
            }
        }

        None
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "function_definition" => {
                // Check if it's a method (inside class)
                if let Some(parent) = node.parent() {
                    if parent.kind() == "block" {
                        if let Some(grandparent) = parent.parent() {
                            if grandparent.kind() == "class_definition" {
                                return ChunkKind::Method;
                            }
                        }
                    }
                }
                ChunkKind::Function
            }
            "class_definition" => ChunkKind::Class,
            _ => ChunkKind::Other,
        }
    }
}

/// TypeScript/JavaScript language extractor
pub struct TypeScriptExtractor;

impl LanguageExtractor for TypeScriptExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &[
            "function_declaration",
            "function",
            "method_definition",
            "class_declaration",
            "class",
            "interface_declaration",
            "type_alias_declaration",
            "enum_declaration",
            // Arrow functions assigned to const
            "lexical_declaration",
            "variable_declaration",
        ]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        // Try name field first
        if let Some(name) = node.child_by_field_name("name") {
            if let Ok(text) = name.utf8_text(source) {
                return Some(text.to_string());
            }
        }

        // For variable declarations, look for identifier
        if node.kind() == "lexical_declaration" || node.kind() == "variable_declaration" {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some(name) = child.child_by_field_name("name") {
                        if let Ok(text) = name.utf8_text(source) {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }

        None
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_declaration" | "function" => {
                let mut sig = String::from("function ");

                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name.utf8_text(source) {
                        sig.push_str(name_text);
                    }
                }

                if let Some(params) = node.child_by_field_name("parameters") {
                    if let Ok(params_text) = params.utf8_text(source) {
                        sig.push_str(params_text);
                    }
                }

                if let Some(return_type) = node.child_by_field_name("return_type") {
                    if let Ok(ret_text) = return_type.utf8_text(source) {
                        sig.push_str(": ");
                        sig.push_str(ret_text);
                    }
                }

                Some(sig)
            }
            "class_declaration" | "class" => {
                let mut sig = String::from("class ");

                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name.utf8_text(source) {
                        sig.push_str(name_text);
                    }
                }

                Some(sig)
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        // Look for JSDoc comments (/** */) before the node
        // Similar to Rust approach
        let parent = node.parent()?;
        let node_index = (0..parent.named_child_count())
            .find(|&i| parent.named_child(i as u32).map(|c| c.id()) == Some(node.id()))?;

        if node_index > 0 {
            if let Some(prev) = parent.named_child((node_index - 1) as u32) {
                if prev.kind() == "comment" {
                    if let Ok(text) = prev.utf8_text(source) {
                        if text.trim_start().starts_with("/**") {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }

        None
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "function_declaration" | "function" => ChunkKind::Function,
            "method_definition" => ChunkKind::Method,
            "class_declaration" | "class" => ChunkKind::Class,
            "interface_declaration" => ChunkKind::Interface,
            "type_alias_declaration" => ChunkKind::TypeAlias,
            "enum_declaration" => ChunkKind::Enum,
            "lexical_declaration" | "variable_declaration" => {
                // Check if it's an arrow function
                // If so, treat as Function, otherwise Other
                ChunkKind::Function
            }
            _ => ChunkKind::Other,
        }
    }
}

/// C language extractor
pub struct CExtractor;

impl LanguageExtractor for CExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &[
            "function_definition",
            "struct_specifier",
            "enum_specifier",
            "type_definition",
        ]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        // For function_definition, name is in the declarator
        if node.kind() == "function_definition" {
            let declarator = node.child_by_field_name("declarator")?;
            // Navigate through pointer_declarator or function_declarator
            return find_identifier(declarator, source);
        }
        // For struct/enum, look for name field or type_identifier child
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok().map(String::from))
            .or_else(|| {
                let mut cursor = node.walk();
                let result = node
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "type_identifier")
                    .and_then(|n| n.utf8_text(source).ok().map(String::from));
                result
            })
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_definition" => {
                // Get everything up to the body
                let body = node.child_by_field_name("body")?;
                let sig_end = body.start_byte();
                let sig_text = std::str::from_utf8(&source[node.start_byte()..sig_end]).ok()?;
                Some(sig_text.trim().to_string())
            }
            "struct_specifier" => {
                let name = self.extract_name(node, source)?;
                Some(format!("struct {}", name))
            }
            "enum_specifier" => {
                let name = self.extract_name(node, source)?;
                Some(format!("enum {}", name))
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        extract_c_style_doc(node, source)
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "function_definition" => ChunkKind::Function,
            "struct_specifier" => ChunkKind::Struct,
            "enum_specifier" => ChunkKind::Enum,
            "type_definition" => ChunkKind::TypeAlias,
            _ => ChunkKind::Other,
        }
    }
}

/// C++ language extractor
pub struct CppExtractor;

impl LanguageExtractor for CppExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &[
            "function_definition",
            "class_specifier",
            "struct_specifier",
            "enum_specifier",
            "namespace_definition",
            "template_declaration",
            "type_definition",
        ]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_definition" => {
                let declarator = node.child_by_field_name("declarator")?;
                find_identifier(declarator, source)
            }
            "namespace_definition" => node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok().map(String::from)),
            "template_declaration" => {
                // Look inside the template for the actual declaration
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if let Some(name) = self.extract_name(child, source) {
                        return Some(name);
                    }
                }
                None
            }
            _ => node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok().map(String::from))
                .or_else(|| {
                    let mut cursor = node.walk();
                    let result = node
                        .named_children(&mut cursor)
                        .find(|c| c.kind() == "type_identifier")
                        .and_then(|n| n.utf8_text(source).ok().map(String::from));
                    result
                }),
        }
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_definition" => {
                let body = node.child_by_field_name("body")?;
                let sig_end = body.start_byte();
                let sig_text = std::str::from_utf8(&source[node.start_byte()..sig_end]).ok()?;
                Some(sig_text.trim().to_string())
            }
            "class_specifier" => {
                let name = self.extract_name(node, source)?;
                Some(format!("class {}", name))
            }
            "struct_specifier" => {
                let name = self.extract_name(node, source)?;
                Some(format!("struct {}", name))
            }
            "namespace_definition" => {
                let name = self.extract_name(node, source).unwrap_or_default();
                Some(format!("namespace {}", name))
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        extract_c_style_doc(node, source)
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "function_definition" => {
                // Check if inside a class
                if let Some(parent) = node.parent() {
                    if parent.kind() == "declaration_list"
                        || parent.kind() == "field_declaration_list"
                    {
                        return ChunkKind::Method;
                    }
                }
                ChunkKind::Function
            }
            "class_specifier" => ChunkKind::Class,
            "struct_specifier" => ChunkKind::Struct,
            "enum_specifier" => ChunkKind::Enum,
            "namespace_definition" => ChunkKind::Mod,
            "template_declaration" => ChunkKind::Other,
            "type_definition" => ChunkKind::TypeAlias,
            _ => ChunkKind::Other,
        }
    }
}

/// C# language extractor
pub struct CSharpExtractor;

impl LanguageExtractor for CSharpExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &[
            "class_declaration",
            "struct_declaration",
            "interface_declaration",
            "method_declaration",
            "constructor_declaration",
            "property_declaration",
            "enum_declaration",
            "namespace_declaration",
            "record_declaration",
        ]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok().map(String::from))
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "method_declaration" | "constructor_declaration" => {
                // Get everything up to the body
                let body = node.child_by_field_name("body");
                let sig_end = body.map(|b| b.start_byte()).unwrap_or(node.end_byte());
                let sig_text = std::str::from_utf8(&source[node.start_byte()..sig_end]).ok()?;
                Some(sig_text.trim().to_string())
            }
            "class_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("class {}", name))
            }
            "struct_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("struct {}", name))
            }
            "interface_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("interface {}", name))
            }
            "enum_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("enum {}", name))
            }
            "namespace_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("namespace {}", name))
            }
            "record_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("record {}", name))
            }
            "property_declaration" => {
                let name = self.extract_name(node, source)?;
                // Try to get the type
                let type_node = node.child_by_field_name("type");
                if let Some(t) = type_node {
                    if let Ok(type_text) = t.utf8_text(source) {
                        return Some(format!("{} {}", type_text, name));
                    }
                }
                Some(name)
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        // C# uses /// XML doc comments
        let parent = node.parent()?;
        let node_index = (0..parent.named_child_count())
            .find(|&i| parent.named_child(i as u32).map(|c| c.id()) == Some(node.id()))?;

        if node_index > 0 {
            if let Some(prev) = parent.named_child((node_index - 1) as u32) {
                if prev.kind() == "comment" {
                    if let Ok(text) = prev.utf8_text(source) {
                        if text.trim_start().starts_with("///") {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "method_declaration" | "constructor_declaration" => ChunkKind::Method,
            "class_declaration" | "record_declaration" => ChunkKind::Class,
            "struct_declaration" => ChunkKind::Struct,
            "interface_declaration" => ChunkKind::Interface,
            "enum_declaration" => ChunkKind::Enum,
            "namespace_declaration" => ChunkKind::Mod,
            "property_declaration" => ChunkKind::Other,
            _ => ChunkKind::Other,
        }
    }
}

/// Go language extractor
pub struct GoExtractor;

impl LanguageExtractor for GoExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &[
            "function_declaration",
            "method_declaration",
            "type_declaration",
            "type_spec",
        ]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok().map(String::from))
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_declaration" => {
                // func name(params) returnType
                let body = node.child_by_field_name("body")?;
                let sig_end = body.start_byte();
                let sig_text = std::str::from_utf8(&source[node.start_byte()..sig_end]).ok()?;
                Some(sig_text.trim().to_string())
            }
            "method_declaration" => {
                let body = node.child_by_field_name("body")?;
                let sig_end = body.start_byte();
                let sig_text = std::str::from_utf8(&source[node.start_byte()..sig_end]).ok()?;
                Some(sig_text.trim().to_string())
            }
            "type_spec" => {
                let name = self.extract_name(node, source)?;
                // Check what type it is (struct_type, interface_type, etc.)
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    match child.kind() {
                        "struct_type" => return Some(format!("type {} struct", name)),
                        "interface_type" => return Some(format!("type {} interface", name)),
                        _ => {}
                    }
                }
                Some(format!("type {}", name))
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        // Go uses // comments before declarations
        let parent = node.parent()?;
        let node_index = (0..parent.named_child_count())
            .find(|&i| parent.named_child(i as u32).map(|c| c.id()) == Some(node.id()))?;

        if node_index > 0 {
            if let Some(prev) = parent.named_child((node_index - 1) as u32) {
                if prev.kind() == "comment" {
                    return prev.utf8_text(source).ok().map(String::from);
                }
            }
        }
        None
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "function_declaration" => ChunkKind::Function,
            "method_declaration" => ChunkKind::Method,
            "type_spec" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    match child.kind() {
                        "struct_type" => return ChunkKind::Struct,
                        "interface_type" => return ChunkKind::Interface,
                        _ => {}
                    }
                }
                ChunkKind::TypeAlias
            }
            "type_declaration" => ChunkKind::TypeAlias,
            _ => ChunkKind::Other,
        }
    }
}

/// Java language extractor
pub struct JavaExtractor;

impl LanguageExtractor for JavaExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &[
            "class_declaration",
            "interface_declaration",
            "method_declaration",
            "constructor_declaration",
            "enum_declaration",
            "annotation_type_declaration",
            "record_declaration",
        ]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok().map(String::from))
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "method_declaration" | "constructor_declaration" => {
                let body = node.child_by_field_name("body");
                let sig_end = body.map(|b| b.start_byte()).unwrap_or(node.end_byte());
                let sig_text = std::str::from_utf8(&source[node.start_byte()..sig_end]).ok()?;
                Some(sig_text.trim().to_string())
            }
            "class_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("class {}", name))
            }
            "interface_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("interface {}", name))
            }
            "enum_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("enum {}", name))
            }
            "record_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("record {}", name))
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        // Java uses /** */ Javadoc comments
        let parent = node.parent()?;
        let node_index = (0..parent.named_child_count())
            .find(|&i| parent.named_child(i as u32).map(|c| c.id()) == Some(node.id()))?;

        if node_index > 0 {
            if let Some(prev) = parent.named_child((node_index - 1) as u32) {
                if prev.kind() == "block_comment" || prev.kind() == "comment" {
                    if let Ok(text) = prev.utf8_text(source) {
                        if text.trim_start().starts_with("/**") {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "method_declaration" | "constructor_declaration" => {
                // Check if inside a class/interface
                if let Some(parent) = node.parent() {
                    if parent.kind() == "class_body" || parent.kind() == "interface_body" {
                        return ChunkKind::Method;
                    }
                }
                ChunkKind::Function
            }
            "class_declaration" | "record_declaration" => ChunkKind::Class,
            "interface_declaration" => ChunkKind::Interface,
            "enum_declaration" => ChunkKind::Enum,
            "annotation_type_declaration" => ChunkKind::Interface,
            _ => ChunkKind::Other,
        }
    }
}

/// Dart language extractor
pub struct DartExtractor;

impl LanguageExtractor for DartExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &[
            "class_declaration",
            "enum_declaration",
            "mixin_declaration",
            "extension_declaration",
            "extension_type_declaration",
            "type_alias",
            "function_declaration",
            "method_declaration",
            "getter_declaration",
            "setter_declaration",
            "constructor_signature",
        ]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            // Types with a direct "name" field
            "class_declaration"
            | "enum_declaration"
            | "mixin_declaration"
            | "extension_declaration"
            | "constructor_signature" => node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok().map(String::from)),
            // Functions/methods/getters/setters: name is inside the "signature" child
            "function_declaration"
            | "method_declaration"
            | "getter_declaration"
            | "setter_declaration" => {
                let sig = node.child_by_field_name("signature")?;
                // function_signature, getter_signature, setter_signature have a "name" field
                if let Some(name) = sig.child_by_field_name("name") {
                    return name.utf8_text(source).ok().map(String::from);
                }
                // method_signature has no named fields — find identifier child
                find_dart_identifier(&sig, source)
            }
            "type_alias" => find_dart_identifier(&node, source),
            "extension_type_declaration" => node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok().map(String::from)),
            _ => None,
        }
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_declaration"
            | "method_declaration"
            | "getter_declaration"
            | "setter_declaration" => {
                // Signature is everything up to the body
                let body = node.child_by_field_name("body");
                let sig_end = body.map(|b| b.start_byte()).unwrap_or(node.end_byte());
                let sig_text = std::str::from_utf8(&source[node.start_byte()..sig_end]).ok()?;
                Some(sig_text.trim().to_string())
            }
            "class_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("class {}", name))
            }
            "enum_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("enum {}", name))
            }
            "mixin_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("mixin {}", name))
            }
            "extension_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("extension {}", name))
            }
            "extension_type_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("extension type {}", name))
            }
            "constructor_signature" => {
                let sig_text =
                    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).ok()?;
                Some(sig_text.trim().to_string())
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        // Dart uses /// doc comments
        let parent = node.parent()?;
        let node_index = (0..parent.named_child_count())
            .find(|&i| parent.named_child(i as u32).map(|c| c.id()) == Some(node.id()))?;

        if node_index > 0 {
            if let Some(prev) = parent.named_child((node_index - 1) as u32) {
                if prev.kind() == "documentation_comment" || prev.kind() == "comment" {
                    if let Ok(text) = prev.utf8_text(source) {
                        let trimmed = text.trim_start();
                        if trimmed.starts_with("///") || trimmed.starts_with("/**") {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "function_declaration" => {
                // A function_declaration nested in a type-member body is a method.
                // Dart's grammar uses `class_body` for the body of classes, mixins
                // AND (via the shared body rule) the member list, so mixin/class
                // methods both land here. `mixin_application` (the `with A, B`
                // superclass clause) is NOT a member body and never parents a
                // function_declaration, so it is intentionally not listed.
                if let Some(parent) = node.parent() {
                    if parent.kind() == "class_body"
                        || parent.kind() == "enum_body"
                        || parent.kind() == "extension_body"
                    {
                        return ChunkKind::Method;
                    }
                }
                ChunkKind::Function
            }
            "method_declaration"
            | "getter_declaration"
            | "setter_declaration"
            | "constructor_signature" => ChunkKind::Method,
            "class_declaration" | "extension_type_declaration" => ChunkKind::Class,
            "enum_declaration" => ChunkKind::Enum,
            "mixin_declaration" => ChunkKind::Class,
            "extension_declaration" => ChunkKind::Other,
            "type_alias" => ChunkKind::TypeAlias,
            _ => ChunkKind::Other,
        }
    }
}

/// Helper: find first `identifier` child in a Dart node (for method_signature, type_alias)
fn find_dart_identifier(node: &Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "identifier" {
            return child.utf8_text(source).ok().map(String::from);
        }
    }
    None
}

/// Haxe language extractor.
///
/// Node kind names come from `themarcocara/tree-sitter-haxe`'s
/// `grammar-declarations.js`, confirmed against `tree-sitter parse` output
/// (not guessed from another language's grammar).
///
/// Known grammar gap: plain (non-`enum`) `abstract Name(UnderlyingType) {}`
/// declarations are not recognized by this grammar as their own node kind —
/// only `enum abstract` (`enum_abstract_declaration`) is. A bare `abstract`
/// type produces an `ERROR` node instead of a `class_declaration`-like node,
/// so such declarations simply won't be picked up as named chunks by
/// `definition_types` below (upstream parser limitation, not something this
/// extractor can special-case around).
pub struct HaxeExtractor;

impl LanguageExtractor for HaxeExtractor {
    fn definition_types(&self) -> &[&'static str] {
        &[
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
            "enum_abstract_declaration",
            "typedef_declaration",
            "function_declaration",
        ]
    }

    fn extract_name(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "enum_abstract_declaration"
            | "typedef_declaration"
            | "function_declaration" => node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok().map(String::from)),
            _ => None,
        }
    }

    fn extract_signature(&self, node: Node, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_declaration" => {
                // Interface method declarations have no body — signature is
                // then the whole node text.
                let body = node.child_by_field_name("body");
                let sig_end = body.map(|b| b.start_byte()).unwrap_or(node.end_byte());
                let sig_text = std::str::from_utf8(&source[node.start_byte()..sig_end]).ok()?;
                Some(sig_text.trim().to_string())
            }
            "class_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("class {}", name))
            }
            "interface_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("interface {}", name))
            }
            "enum_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("enum {}", name))
            }
            "enum_abstract_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("enum abstract {}", name))
            }
            "typedef_declaration" => {
                let name = self.extract_name(node, source)?;
                Some(format!("typedef {}", name))
            }
            _ => None,
        }
    }

    fn extract_docstring(&self, node: Node, source: &[u8]) -> Option<String> {
        // Haxe (dox) doc comments are `/** ... */` immediately preceding the
        // declaration — there is no `///` convention like Dart/Rust.
        let parent = node.parent()?;
        let node_index = (0..parent.named_child_count())
            .find(|&i| parent.named_child(i as u32).map(|c| c.id()) == Some(node.id()))?;

        if node_index > 0 {
            if let Some(prev) = parent.named_child((node_index - 1) as u32) {
                if prev.kind() == "comment" {
                    if let Ok(text) = prev.utf8_text(source) {
                        if text.trim_start().starts_with("/**") {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    fn classify(&self, node: Node) -> ChunkKind {
        match node.kind() {
            "function_declaration" => {
                // Class/interface/enum/enum-abstract bodies AND function
                // bodies all share the same generic "block" node kind in
                // this grammar (no distinct class_body/function_body split
                // like Dart has) — so a function_declaration's immediate
                // parent being "block" is not enough on its own to tell a
                // method apart from a locally-nested function. Check the
                // block's own parent (the grandparent of this node) to see
                // whether the block IS a type declaration's body.
                if let Some(block) = node.parent() {
                    if block.kind() == "block" {
                        if let Some(owner) = block.parent() {
                            if matches!(
                                owner.kind(),
                                "class_declaration"
                                    | "interface_declaration"
                                    | "enum_declaration"
                                    | "enum_abstract_declaration"
                            ) {
                                return ChunkKind::Method;
                            }
                        }
                    }
                }
                ChunkKind::Function
            }
            "class_declaration" => ChunkKind::Class,
            "interface_declaration" => ChunkKind::Interface,
            "enum_declaration" | "enum_abstract_declaration" => ChunkKind::Enum,
            "typedef_declaration" => ChunkKind::TypeAlias,
            _ => ChunkKind::Other,
        }
    }
}

/// Helper: recursively find the first identifier in a declarator chain (for C/C++)
fn find_identifier(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() == "identifier"
        || node.kind() == "field_identifier"
        || node.kind() == "destructor_name"
    {
        return node.utf8_text(source).ok().map(String::from);
    }
    // For qualified identifiers like ClassName::method
    if node.kind() == "qualified_identifier" || node.kind() == "scoped_identifier" {
        return node.utf8_text(source).ok().map(String::from);
    }
    // Recurse into declarator children
    if let Some(declarator) = node.child_by_field_name("declarator") {
        return find_identifier(declarator, source);
    }
    // Try named children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(name) = find_identifier(child, source) {
            return Some(name);
        }
    }
    None
}

/// Helper: extract C-style doc comments (/** */ or ///) before a node
fn extract_c_style_doc(node: Node, source: &[u8]) -> Option<String> {
    let parent = node.parent()?;
    let node_index = (0..parent.named_child_count())
        .find(|&i| parent.named_child(i as u32).map(|c| c.id()) == Some(node.id()))?;

    if node_index > 0 {
        if let Some(prev) = parent.named_child((node_index - 1) as u32) {
            if prev.kind() == "comment" || prev.kind() == "block_comment" {
                if let Ok(text) = prev.utf8_text(source) {
                    let trimmed = text.trim_start();
                    if trimmed.starts_with("///") || trimmed.starts_with("/**") {
                        return Some(text.to_string());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_extractor() {
        assert!(get_extractor(Language::Rust).is_some());
        assert!(get_extractor(Language::Python).is_some());
        assert!(get_extractor(Language::JavaScript).is_some());
        assert!(get_extractor(Language::TypeScript).is_some());
        assert!(get_extractor(Language::C).is_some());
        assert!(get_extractor(Language::Cpp).is_some());
        assert!(get_extractor(Language::CSharp).is_some());
        assert!(get_extractor(Language::Go).is_some());
        assert!(get_extractor(Language::Java).is_some());
        assert!(get_extractor(Language::Markdown).is_none());
    }

    #[test]
    fn test_rust_definition_types() {
        let extractor = RustExtractor;
        let types = extractor.definition_types();

        assert!(types.contains(&"function_item"));
        assert!(types.contains(&"struct_item"));
        assert!(types.contains(&"enum_item"));
        assert!(types.contains(&"impl_item"));
    }

    #[test]
    fn test_python_definition_types() {
        let extractor = PythonExtractor;
        let types = extractor.definition_types();

        assert!(types.contains(&"function_definition"));
        assert!(types.contains(&"class_definition"));
    }

    #[test]
    fn test_haxe_definition_types() {
        let extractor = HaxeExtractor;
        let types = extractor.definition_types();

        assert!(types.contains(&"class_declaration"));
        assert!(types.contains(&"interface_declaration"));
        assert!(types.contains(&"enum_declaration"));
        assert!(types.contains(&"enum_abstract_declaration"));
        assert!(types.contains(&"typedef_declaration"));
        assert!(types.contains(&"function_declaration"));
    }

    /// Parse Haxe source and return the tree alongside the source bytes
    /// (the tree borrows nothing from `source`, so both must stay alive
    /// together for the caller to walk it).
    fn parse_haxe(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_haxe::LANGUAGE.into())
            .expect("failed to load Haxe grammar");
        let tree = parser.parse(source, None).expect("failed to parse Haxe source");
        (tree, source.as_bytes().to_vec())
    }

    /// Depth-first search for the first node of the given kind.
    fn find_node<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_node(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_haxe_class_and_method() {
        let source = r#"
/**
 * A field definition.
 */
class FieldDefinition {
  /** Validates the field. */
  public function validate():Bool {
    return true;
  }
}
"#;
        let (tree, source) = parse_haxe(source);
        let extractor = HaxeExtractor;

        let class_node = find_node(tree.root_node(), "class_declaration").unwrap();
        assert_eq!(
            extractor.extract_name(class_node, &source),
            Some("FieldDefinition".to_string())
        );
        assert_eq!(extractor.classify(class_node), ChunkKind::Class);
        assert_eq!(
            extractor.extract_signature(class_node, &source),
            Some("class FieldDefinition".to_string())
        );
        let class_doc = extractor.extract_docstring(class_node, &source).unwrap();
        assert!(class_doc.contains("A field definition."));

        let method_node = find_node(tree.root_node(), "function_declaration").unwrap();
        assert_eq!(
            extractor.extract_name(method_node, &source),
            Some("validate".to_string())
        );
        assert_eq!(extractor.classify(method_node), ChunkKind::Method);
        let method_doc = extractor.extract_docstring(method_node, &source).unwrap();
        assert!(method_doc.contains("Validates the field."));
    }

    #[test]
    fn test_haxe_top_level_function_vs_nested() {
        let source = r#"
function topLevelFn(x:Int):Int {
  function helper():Int {
    return 1;
  }
  return helper();
}
"#;
        let (tree, source) = parse_haxe(source);
        let extractor = HaxeExtractor;

        let top_fn = find_node(tree.root_node(), "function_declaration").unwrap();
        assert_eq!(
            extractor.extract_name(top_fn, &source),
            Some("topLevelFn".to_string())
        );
        // Top-level function: not nested in a class/interface/enum body block.
        assert_eq!(extractor.classify(top_fn), ChunkKind::Function);

        // The nested `helper` function is still a Function (not a Method) —
        // it's local to another function's body, not a type member.
        let nested_fn = find_node(top_fn.child_by_field_name("body").unwrap(), "function_declaration").unwrap();
        assert_eq!(
            extractor.extract_name(nested_fn, &source),
            Some("helper".to_string())
        );
        assert_eq!(extractor.classify(nested_fn), ChunkKind::Function);
    }

    #[test]
    fn test_haxe_constructor_name() {
        // Regression check: the grammar's `function_declaration` rule types
        // its `name` field as `choice($._lhs_expression, 'new')` — the `'new'`
        // keyword alternative is an anonymous token, not a named identifier
        // node, and doesn't show up in a named-nodes-only tree dump. Confirm
        // `child_by_field_name` still resolves it regardless.
        let source = r#"
class Foo {
  public function new(x:Int) {
    this.x = x;
  }
}
"#;
        let (tree, source) = parse_haxe(source);
        let extractor = HaxeExtractor;

        let ctor = find_node(tree.root_node(), "function_declaration").unwrap();
        assert_eq!(
            extractor.extract_name(ctor, &source),
            Some("new".to_string())
        );
        assert_eq!(extractor.classify(ctor), ChunkKind::Method);
    }

    #[test]
    fn test_haxe_interface_enum_typedef() {
        let source = r#"
interface Validatable {
  function validate():Bool;
}

enum Color {
  Red;
  Green;
  Blue;
}

enum abstract Status(Int) from Int to Int {
  var Active = 0;
  var Inactive = 1;
}

typedef Point = {
  var x:Int;
  var y:Int;
}
"#;
        let (tree, source) = parse_haxe(source);
        let extractor = HaxeExtractor;

        let iface = find_node(tree.root_node(), "interface_declaration").unwrap();
        assert_eq!(
            extractor.extract_name(iface, &source),
            Some("Validatable".to_string())
        );
        assert_eq!(extractor.classify(iface), ChunkKind::Interface);

        // Interface methods have no body — signature is the whole node text.
        let iface_method = find_node(iface, "function_declaration").unwrap();
        assert_eq!(
            extractor.extract_signature(iface_method, &source),
            Some("function validate():Bool;".to_string())
        );

        let enum_node = find_node(tree.root_node(), "enum_declaration").unwrap();
        assert_eq!(
            extractor.extract_name(enum_node, &source),
            Some("Color".to_string())
        );
        assert_eq!(extractor.classify(enum_node), ChunkKind::Enum);

        let enum_abstract = find_node(tree.root_node(), "enum_abstract_declaration").unwrap();
        assert_eq!(
            extractor.extract_name(enum_abstract, &source),
            Some("Status".to_string())
        );
        assert_eq!(extractor.classify(enum_abstract), ChunkKind::Enum);

        let typedef_node = find_node(tree.root_node(), "typedef_declaration").unwrap();
        assert_eq!(
            extractor.extract_name(typedef_node, &source),
            Some("Point".to_string())
        );
        assert_eq!(extractor.classify(typedef_node), ChunkKind::TypeAlias);
    }
}
