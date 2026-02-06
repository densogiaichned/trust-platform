//! Shared utilities for IDE features.
//!
//! This module provides common functionality used across multiple IDE features.

use smol_str::SmolStr;
use std::sync::Arc;
use text_size::{TextRange, TextSize};

use rustc_hash::FxHashSet;
use trust_hir::db::{FileId, SemanticDatabase};
use trust_hir::symbols::{ScopeId, Symbol, SymbolKind, SymbolTable};
use trust_hir::{Database, SourceDatabase, SymbolId, Type, TypeId};
use trust_syntax::parser::parse;
use trust_syntax::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use trust_syntax::{lex, TokenKind};

/// Finds the enclosing POU (Program Organization Unit) node for a given position.
pub fn find_enclosing_pou(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxNode> {
    let token = root.token_at_offset(offset).right_biased()?;
    token
        .parent_ancestors()
        .find(|node| is_pou_kind(node.kind()))
}

/// Checks if a syntax kind is a POU.
pub fn is_pou_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Program
            | SyntaxKind::Function
            | SyntaxKind::FunctionBlock
            | SyntaxKind::Class
            | SyntaxKind::Method
            | SyntaxKind::Property
            | SyntaxKind::Interface
    )
}

/// Checks if a symbol kind represents a POU.
pub fn is_pou_symbol_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Program
            | SymbolKind::Function { .. }
            | SymbolKind::FunctionBlock
            | SymbolKind::Class
            | SymbolKind::Method { .. }
            | SymbolKind::Property { .. }
            | SymbolKind::Interface
    )
}

/// Checks if a symbol kind represents a type declaration.
pub(crate) fn is_type_symbol_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Type | SymbolKind::FunctionBlock | SymbolKind::Class | SymbolKind::Interface
    )
}

/// Checks if a symbol kind is a member of a type (field, method, property, etc.).
pub(crate) fn is_member_symbol_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Variable { .. }
            | SymbolKind::Constant
            | SymbolKind::Method { .. }
            | SymbolKind::Property { .. }
            | SymbolKind::Function { .. }
    )
}

/// Gets the scope ID for a POU node.
pub fn scope_for_pou(symbols: &SymbolTable, pou_node: &SyntaxNode) -> ScopeId {
    // Get the POU name
    let pou_name = pou_node
        .children()
        .find(|n| n.kind() == SyntaxKind::Name)
        .and_then(|n| {
            n.descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == SyntaxKind::Ident)
        })
        .map(|t| t.text().to_string());

    let Some(name) = pou_name else {
        return ScopeId::GLOBAL;
    };

    // Find the symbol for this POU
    let pou_symbol = symbols
        .iter()
        .find(|sym| sym.name.eq_ignore_ascii_case(&name) && is_pou_symbol_kind(&sym.kind));

    let Some(pou_sym) = pou_symbol else {
        return ScopeId::GLOBAL;
    };

    // Find the scope owned by this symbol
    for i in 0..symbols.scope_count() {
        let scope_id = ScopeId(i as u32);
        if let Some(scope) = symbols.get_scope(scope_id) {
            if scope.owner == Some(pou_sym.id) {
                return scope_id;
            }
        }
    }

    ScopeId::GLOBAL
}

/// Finds the scope ID at a given position.
pub fn scope_at_position(symbols: &SymbolTable, root: &SyntaxNode, offset: TextSize) -> ScopeId {
    if let Some(pou_node) = find_enclosing_pou(root, offset) {
        scope_for_pou(symbols, &pou_node)
    } else if let Some(scope_id) = scope_for_namespace(symbols, root, offset) {
        scope_id
    } else {
        ScopeId::GLOBAL
    }
}

/// Finds the identifier at a given offset in the source text.
pub fn ident_at_offset(source: &str, offset: TextSize) -> Option<(&str, TextRange)> {
    let offset = u32::from(offset) as usize;
    for token in lex(source) {
        if token.kind != TokenKind::Ident {
            continue;
        }
        let start = usize::from(token.range.start());
        let end = usize::from(token.range.end());
        if start <= offset && offset < end {
            return Some((&source[start..end], token.range));
        }
    }
    if offset == 0 {
        return None;
    }
    let fallback = offset - 1;
    for token in lex(source) {
        if token.kind != TokenKind::Ident {
            continue;
        }
        let start = usize::from(token.range.start());
        let end = usize::from(token.range.end());
        if start <= fallback && fallback < end {
            return Some((&source[start..end], token.range));
        }
    }
    None
}

#[derive(Debug, Clone)]
pub(crate) struct FieldTarget {
    pub(crate) type_id: TypeId,
    pub(crate) name: SmolStr,
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedTarget {
    Symbol(SymbolId),
    Field(FieldTarget),
}

pub(crate) struct IdeContext<'a> {
    pub(crate) db: &'a Database,
    pub(crate) file_id: FileId,
    pub(crate) source: Arc<String>,
    pub(crate) root: SyntaxNode,
    pub(crate) symbols: Arc<SymbolTable>,
}

impl<'a> IdeContext<'a> {
    pub(crate) fn new(db: &'a Database, file_id: FileId) -> Self {
        let source = db.source_text(file_id);
        let parsed = parse(&source);
        let root = parsed.syntax();
        let symbols = db.file_symbols_with_project(file_id);
        Self {
            db,
            file_id,
            source,
            root,
            symbols,
        }
    }

    pub(crate) fn resolve_target_at_position(&self, position: TextSize) -> Option<ResolvedTarget> {
        resolve_target_at_position_with_context(
            self.db,
            self.file_id,
            position,
            &self.source,
            &self.root,
            &self.symbols,
        )
    }

    pub(crate) fn scope_at_position(&self, position: TextSize) -> ScopeId {
        scope_at_position(&self.symbols, &self.root, position)
    }
}

pub(crate) struct SymbolFilter<'a> {
    symbols: &'a SymbolTable,
}

impl<'a> SymbolFilter<'a> {
    pub(crate) fn new(symbols: &'a SymbolTable) -> Self {
        Self { symbols }
    }

    pub(crate) fn symbols(&self) -> &'a SymbolTable {
        self.symbols
    }

    pub(crate) fn scope_symbols(&self, scope_id: ScopeId) -> Vec<&'a Symbol> {
        let mut items = Vec::new();
        let mut seen: FxHashSet<String> = FxHashSet::default();
        let mut current = Some(scope_id);

        while let Some(scope_id) = current {
            let Some(scope) = self.symbols.get_scope(scope_id) else {
                break;
            };
            for symbol_id in scope.symbol_ids() {
                let Some(symbol) = self.symbols.get(*symbol_id) else {
                    continue;
                };
                if !seen.insert(symbol.name.to_ascii_uppercase()) {
                    continue;
                }
                items.push(symbol);
            }
            current = scope.parent;
        }

        items
    }

    pub(crate) fn symbol_at_range(&self, range: TextRange) -> Option<&'a Symbol> {
        self.symbols.iter().find(|sym| sym.range == range)
    }

    pub(crate) fn resolve_in_scope(&self, name: &str, scope_id: ScopeId) -> Option<&'a Symbol> {
        self.symbols
            .resolve(name, scope_id)
            .and_then(|symbol_id| self.symbols.get(symbol_id))
    }

    pub(crate) fn lookup_any(&self, name: &str) -> Option<&'a Symbol> {
        self.symbols
            .lookup_any(name)
            .and_then(|symbol_id| self.symbols.get(symbol_id))
    }

    pub(crate) fn type_symbols(&self) -> impl Iterator<Item = &'a Symbol> {
        self.symbols.iter().filter(|symbol| {
            matches!(
                symbol.kind,
                SymbolKind::Type
                    | SymbolKind::FunctionBlock
                    | SymbolKind::Class
                    | SymbolKind::Interface
            )
        })
    }

    pub(crate) fn symbol_with_type_id<F>(&self, type_id: TypeId, predicate: F) -> Option<&'a Symbol>
    where
        F: Fn(&Symbol) -> bool,
    {
        self.symbols
            .iter()
            .find(|sym| sym.type_id == type_id && predicate(sym))
    }

    pub(crate) fn owner_for_type(&self, type_id: TypeId) -> Option<SymbolId> {
        self.symbols
            .iter()
            .find(|sym| {
                sym.type_id == type_id
                    && matches!(
                        sym.kind,
                        SymbolKind::FunctionBlock | SymbolKind::Class | SymbolKind::Interface
                    )
            })
            .map(|sym| sym.id)
    }

    pub(crate) fn members_of_owner(&self, owner_id: SymbolId) -> impl Iterator<Item = &'a Symbol> {
        self.symbols
            .iter()
            .filter(move |sym| sym.parent == Some(owner_id))
    }

    pub(crate) fn members_in_hierarchy<F>(
        &self,
        owner_id: SymbolId,
        mut predicate: F,
    ) -> Vec<&'a Symbol>
    where
        F: FnMut(&Symbol) -> bool,
    {
        let mut items = Vec::new();
        let mut seen: FxHashSet<String> = FxHashSet::default();
        let mut current = Some(owner_id);

        while let Some(owner_id) = current {
            for symbol in self
                .symbols
                .iter()
                .filter(|sym| sym.parent == Some(owner_id))
            {
                if !predicate(symbol) {
                    continue;
                }
                if !seen.insert(symbol.name.to_ascii_uppercase()) {
                    continue;
                }
                items.push(symbol);
            }
            let base_name = self.symbols.extends_name(owner_id).cloned();
            current = base_name.and_then(|name| self.symbols.resolve_by_name(name.as_str()));
        }

        items
    }
}

pub(crate) fn resolve_target_at_position(
    db: &Database,
    file_id: FileId,
    position: TextSize,
) -> Option<ResolvedTarget> {
    let context = IdeContext::new(db, file_id);
    context.resolve_target_at_position(position)
}

/// Returns the resolved symbol name at the given position, if any.
pub fn symbol_name_at_position(
    db: &Database,
    file_id: FileId,
    position: TextSize,
) -> Option<SmolStr> {
    let target = resolve_target_at_position(db, file_id, position)?;
    let ResolvedTarget::Symbol(symbol_id) = target else {
        return None;
    };
    let symbols = db.file_symbols_with_project(file_id);
    symbols.get(symbol_id).map(|symbol| symbol.name.clone())
}

pub(crate) fn resolve_target_at_position_with_context(
    db: &Database,
    file_id: FileId,
    position: TextSize,
    source: &str,
    root: &SyntaxNode,
    symbols: &SymbolTable,
) -> Option<ResolvedTarget> {
    let (name, range) = ident_at_offset(source, position)?;
    let scope_id = scope_at_position(symbols, root, position);

    if let Some(symbol) = symbols.iter().find(|sym| {
        if sym.range != range {
            return false;
        }
        match sym.origin {
            Some(origin) => origin.file_id == file_id,
            None => true,
        }
    }) {
        return Some(ResolvedTarget::Symbol(symbol.id));
    }

    let token = root.token_at_offset(position).right_biased()?;
    let name_node = name_node_at_token(&token)?;

    if name_node.kind() == SyntaxKind::Name {
        if let Some(field_target) = resolve_field_decl_target(symbols, &name_node, name) {
            return Some(ResolvedTarget::Field(field_target));
        }

        if let Some(target) = resolve_field_target(db, file_id, symbols, &name_node, name) {
            return Some(target);
        }

        if let Some(field_expr) = name_node
            .parent()
            .filter(|parent| parent.kind() == SyntaxKind::FieldExpr)
        {
            if let Some(parts) = qualified_name_from_field_expr(&field_expr) {
                if let Some(symbol_id) = symbols.resolve_qualified(&parts) {
                    return Some(ResolvedTarget::Symbol(symbol_id));
                }
            }
        }

        if is_type_name_node(&name_node) {
            if let Some(parts) = qualified_name_parts_from_node(&name_node) {
                if let Some(symbol_id) = resolve_type_symbol(symbols, &parts, scope_id) {
                    return Some(ResolvedTarget::Symbol(symbol_id));
                }
            } else if let Some(symbol_id) =
                resolve_type_symbol(symbols, &[SmolStr::new(name)], scope_id)
            {
                return Some(ResolvedTarget::Symbol(symbol_id));
            }
        }
    } else if name_node.kind() == SyntaxKind::NameRef {
        if let Some(target) = resolve_field_target(db, file_id, symbols, &name_node, name) {
            return Some(target);
        }

        if let Some(field_expr) = name_node
            .parent()
            .filter(|parent| parent.kind() == SyntaxKind::FieldExpr)
        {
            if let Some(parts) = qualified_name_from_field_expr(&field_expr) {
                if let Some(symbol_id) = symbols.resolve_qualified(&parts) {
                    return Some(ResolvedTarget::Symbol(symbol_id));
                }
            }
        }
    }

    symbols.resolve(name, scope_id).map(ResolvedTarget::Symbol)
}
fn name_node_at_token(token: &SyntaxToken) -> Option<SyntaxNode> {
    token
        .parent_ancestors()
        .find(|n| matches!(n.kind(), SyntaxKind::Name | SyntaxKind::NameRef))
}

pub(crate) fn is_type_name_node(name_node: &SyntaxNode) -> bool {
    name_node.ancestors().skip(1).any(|n| {
        matches!(
            n.kind(),
            SyntaxKind::TypeRef | SyntaxKind::ExtendsClause | SyntaxKind::ImplementsClause
        )
    })
}

pub(crate) fn resolve_type_symbol(
    symbols: &SymbolTable,
    parts: &[SmolStr],
    scope_id: ScopeId,
) -> Option<SymbolId> {
    if parts.is_empty() {
        return None;
    }
    if parts.len() > 1 {
        let symbol_id = symbols.resolve_qualified(parts)?;
        return symbols
            .get(symbol_id)
            .filter(|sym| sym.is_type())
            .map(|sym| sym.id);
    }
    if let Some(symbol_id) = symbols.resolve(parts[0].as_str(), scope_id) {
        if let Some(symbol) = symbols.get(symbol_id) {
            if symbol.is_type() {
                return Some(symbol_id);
            }
        }
    }
    let type_id = symbols.lookup_type(parts[0].as_str())?;
    symbols
        .iter()
        .find(|sym| sym.is_type() && sym.type_id == type_id)
        .map(|sym| sym.id)
}

pub(crate) fn resolve_type_symbol_at_node(
    symbols: &SymbolTable,
    root: &SyntaxNode,
    name_node: &SyntaxNode,
) -> Option<SymbolId> {
    let parts = qualified_name_parts_from_node(name_node)?;
    if parts.is_empty() {
        return None;
    }
    let scope_id = scope_at_position(symbols, root, name_node.text_range().start());
    resolve_type_symbol(symbols, &parts, scope_id)
}

pub(crate) fn qualified_name_parts_from_node(node: &SyntaxNode) -> Option<Vec<SmolStr>> {
    let target = match node.kind() {
        SyntaxKind::QualifiedName => node.clone(),
        SyntaxKind::Name => {
            if let Some(parent) = node.parent() {
                if parent.kind() == SyntaxKind::QualifiedName {
                    parent
                } else {
                    node.clone()
                }
            } else {
                node.clone()
            }
        }
        _ => return None,
    };

    match target.kind() {
        SyntaxKind::Name => name_from_name_node(&target).map(|name| vec![name]),
        SyntaxKind::QualifiedName => {
            let mut parts = Vec::new();
            for child in target.children().filter(|n| n.kind() == SyntaxKind::Name) {
                if let Some(name) = name_from_name_node(&child) {
                    parts.push(name);
                }
            }
            (!parts.is_empty()).then_some(parts)
        }
        _ => None,
    }
}

pub(crate) fn qualified_name_from_field_expr(node: &SyntaxNode) -> Option<Vec<SmolStr>> {
    if node.kind() != SyntaxKind::FieldExpr {
        return None;
    }
    let mut parts: Vec<SmolStr> = Vec::new();
    let mut current = node.clone();
    loop {
        let mut children = current.children();
        let base = children.next()?;
        let member = children.next()?;
        let member_name = name_from_name_ref(&member)?;
        parts.push(member_name);
        match base.kind() {
            SyntaxKind::FieldExpr => {
                current = base;
            }
            SyntaxKind::NameRef => {
                let base_name = name_from_name_ref(&base)?;
                parts.push(base_name);
                break;
            }
            _ => return None,
        }
    }
    parts.reverse();
    Some(parts)
}

pub(crate) fn name_from_name_ref(node: &SyntaxNode) -> Option<SmolStr> {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| SmolStr::new(t.text()))
}

pub(crate) fn name_from_name_node(node: &SyntaxNode) -> Option<SmolStr> {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| SmolStr::new(t.text()))
}

pub(crate) fn namespace_path_for_symbol(symbols: &SymbolTable, symbol: &Symbol) -> Vec<SmolStr> {
    let mut parts = Vec::new();
    let mut current = symbol.parent;
    while let Some(parent_id) = current {
        let Some(parent) = symbols.get(parent_id) else {
            break;
        };
        if matches!(parent.kind, SymbolKind::Namespace) {
            parts.push(parent.name.clone());
        }
        current = parent.parent;
    }
    parts.reverse();
    parts
}

pub(crate) fn using_path_for_symbol(
    symbols: &SymbolTable,
    scope_id: ScopeId,
    name: &str,
    symbol_id: SymbolId,
) -> Option<Vec<SmolStr>> {
    let mut current = Some(scope_id);
    while let Some(scope_id) = current {
        let Some(scope) = symbols.get_scope(scope_id) else {
            break;
        };
        if scope.lookup_local(name).is_some() {
            return None;
        }

        let mut match_path: Option<Vec<SmolStr>> = None;
        for using in &scope.using_directives {
            let mut parts = using.path.clone();
            parts.push(SmolStr::new(name));
            let Some(target_id) = symbols.resolve_qualified(&parts) else {
                continue;
            };
            if target_id != symbol_id {
                continue;
            }
            if match_path.is_some() {
                return None;
            }
            match_path = Some(using.path.clone());
        }

        if match_path.is_some() {
            return match_path;
        }

        current = scope.parent;
    }
    None
}

pub(crate) fn name_range_from_node(node: &SyntaxNode) -> Option<TextRange> {
    if node.kind() == SyntaxKind::Name {
        return ident_token_in_name(node).map(|token| token.text_range());
    }

    node.children()
        .find(|child| child.kind() == SyntaxKind::Name)
        .and_then(|child| ident_token_in_name(&child))
        .map(|token| token.text_range())
}

fn scope_for_namespace(
    symbols: &SymbolTable,
    root: &SyntaxNode,
    offset: TextSize,
) -> Option<ScopeId> {
    let token = root.token_at_offset(offset).right_biased()?;
    let mut namespaces: Vec<SyntaxNode> = token
        .parent_ancestors()
        .filter(|node| node.kind() == SyntaxKind::Namespace)
        .collect();
    if namespaces.is_empty() {
        return None;
    }

    namespaces.reverse();
    let mut scope_id = ScopeId::GLOBAL;
    for namespace in namespaces {
        let parts = namespace_name_parts(&namespace);
        if parts.is_empty() {
            continue;
        }
        for part in parts {
            let symbol_id = symbols.resolve(part.as_str(), scope_id)?;
            let symbol = symbols.get(symbol_id)?;
            if !matches!(symbol.kind, SymbolKind::Namespace) {
                return None;
            }
            scope_id = symbols.scope_for_owner(symbol_id)?;
        }
    }

    Some(scope_id)
}

fn namespace_name_parts(node: &SyntaxNode) -> Vec<SmolStr> {
    let Some(name_node) = node
        .children()
        .find(|child| matches!(child.kind(), SyntaxKind::Name | SyntaxKind::QualifiedName))
    else {
        return Vec::new();
    };

    match name_node.kind() {
        SyntaxKind::Name => name_from_name_node(&name_node).into_iter().collect(),
        SyntaxKind::QualifiedName => name_node
            .children()
            .filter(|child| child.kind() == SyntaxKind::Name)
            .filter_map(|child| name_from_name_node(&child))
            .collect(),
        _ => Vec::new(),
    }
}

fn resolve_field_target(
    db: &Database,
    file_id: FileId,
    symbols: &SymbolTable,
    name_node: &SyntaxNode,
    field_name: &str,
) -> Option<ResolvedTarget> {
    let field_expr = name_node.parent()?;
    if field_expr.kind() != SyntaxKind::FieldExpr {
        return None;
    }

    let base_expr = field_expr.children().next()?;
    let base_type = expression_type_at_node(db, file_id, &base_expr)?;
    let base_type = symbols.resolve_alias_type(base_type);

    if let Some(member_id) = symbols.resolve_member_symbol_in_type(base_type, field_name) {
        return Some(ResolvedTarget::Symbol(member_id));
    }

    if let Some(field_target) = resolve_struct_field(symbols, base_type, field_name) {
        return Some(ResolvedTarget::Field(field_target));
    }

    None
}

fn resolve_field_decl_target(
    symbols: &SymbolTable,
    name_node: &SyntaxNode,
    field_name: &str,
) -> Option<FieldTarget> {
    if name_node.parent()?.kind() != SyntaxKind::VarDecl {
        return None;
    }
    let type_decl = name_node
        .ancestors()
        .skip(1)
        .find(|n| n.kind() == SyntaxKind::TypeDecl)?;
    let type_name_node = type_decl
        .children()
        .find(|n| n.kind() == SyntaxKind::Name)?;
    let type_ident = ident_token_in_name(&type_name_node)?;
    let type_id = symbols.lookup_type(type_ident.text())?;
    let type_id = symbols.resolve_alias_type(type_id);

    match symbols.type_by_id(type_id)? {
        Type::Struct { .. } | Type::Union { .. } => Some(FieldTarget {
            type_id,
            name: SmolStr::new(field_name),
        }),
        _ => None,
    }
}

fn resolve_struct_field(
    symbols: &SymbolTable,
    type_id: TypeId,
    field_name: &str,
) -> Option<FieldTarget> {
    match symbols.type_by_id(type_id)? {
        Type::Struct { fields, .. } => fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(field_name))
            .map(|field| FieldTarget {
                type_id,
                name: field.name.clone(),
            }),
        Type::Union { variants, .. } => variants
            .iter()
            .find(|variant| variant.name.eq_ignore_ascii_case(field_name))
            .map(|variant| FieldTarget {
                type_id,
                name: variant.name.clone(),
            }),
        _ => None,
    }
}

fn expression_type_at_node(db: &Database, file_id: FileId, node: &SyntaxNode) -> Option<TypeId> {
    let offset = u32::from(node.text_range().start());
    let expr_id = db.expr_id_at_offset(file_id, offset)?;
    Some(db.type_of(file_id, expr_id))
}

pub(crate) fn field_type(symbols: &SymbolTable, target: &FieldTarget) -> Option<TypeId> {
    match symbols.type_by_id(target.type_id)? {
        Type::Struct { fields, .. } => fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(&target.name))
            .map(|field| field.type_id),
        Type::Union { variants, .. } => variants
            .iter()
            .find(|variant| variant.name.eq_ignore_ascii_case(&target.name))
            .map(|variant| variant.type_id),
        _ => None,
    }
}

pub(crate) fn type_detail(symbols: &SymbolTable, type_id: TypeId) -> Option<SmolStr> {
    symbols.type_name(type_id)
}

pub(crate) fn field_declaration_ranges(
    root: &SyntaxNode,
    symbols: &SymbolTable,
    target: &FieldTarget,
) -> Vec<TextRange> {
    let Some(type_name) = symbols.type_name(target.type_id) else {
        return Vec::new();
    };

    let mut ranges = Vec::new();
    for type_decl in root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::TypeDecl)
    {
        let Some(type_name_node) = type_decl.children().find(|n| n.kind() == SyntaxKind::Name)
        else {
            continue;
        };
        let Some(type_ident) = ident_token_in_name(&type_name_node) else {
            continue;
        };
        if !type_ident.text().eq_ignore_ascii_case(&type_name) {
            continue;
        }

        let struct_def = type_decl
            .children()
            .find(|n| matches!(n.kind(), SyntaxKind::StructDef | SyntaxKind::UnionDef));
        let Some(struct_def) = struct_def else {
            continue;
        };

        for var_decl in struct_def
            .children()
            .filter(|n| n.kind() == SyntaxKind::VarDecl)
        {
            for name_node in var_decl.children().filter(|n| n.kind() == SyntaxKind::Name) {
                let Some(ident) = ident_token_in_name(&name_node) else {
                    continue;
                };
                if ident.text().eq_ignore_ascii_case(&target.name) {
                    ranges.push(ident.text_range());
                }
            }
        }
    }

    ranges
}

pub(crate) fn ident_token_in_name(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == SyntaxKind::Ident)
}
