//! Bytecode validation.

#![allow(missing_docs)]

use std::collections::HashSet;

use super::reader::BytecodeReader;
use super::{
    BytecodeError, BytecodeModule, ConstEntry, ConstPool, DebugMap, IoMap, PouIndex, PouKind,
    RefSegment, RefTable, ResourceMeta, RetainInit, SectionData, SectionId, StringTable, TypeData,
    TypeEntry, TypeKind, TypeTable, VarMeta,
};

impl BytecodeModule {
    pub fn validate(&self) -> Result<(), BytecodeError> {
        let strings = match self.section(SectionId::StringTable) {
            Some(SectionData::StringTable(table)) => table,
            _ => return Err(BytecodeError::MissingSection("STRING_TABLE".into())),
        };
        let debug_strings = match self.section(SectionId::DebugStringTable) {
            Some(SectionData::DebugStringTable(table)) => Some(table),
            _ => None,
        };
        let types = match self.section(SectionId::TypeTable) {
            Some(SectionData::TypeTable(table)) => table,
            _ => return Err(BytecodeError::MissingSection("TYPE_TABLE".into())),
        };
        let const_pool = match self.section(SectionId::ConstPool) {
            Some(SectionData::ConstPool(pool)) => pool,
            _ => return Err(BytecodeError::MissingSection("CONST_POOL".into())),
        };
        let ref_table = match self.section(SectionId::RefTable) {
            Some(SectionData::RefTable(table)) => table,
            _ => return Err(BytecodeError::MissingSection("REF_TABLE".into())),
        };
        let pou_index = match self.section(SectionId::PouIndex) {
            Some(SectionData::PouIndex(index)) => index,
            _ => return Err(BytecodeError::MissingSection("POU_INDEX".into())),
        };
        let pou_bodies = match self.section(SectionId::PouBodies) {
            Some(SectionData::PouBodies(bodies)) => bodies,
            _ => return Err(BytecodeError::MissingSection("POU_BODIES".into())),
        };
        let resource_meta = match self.section(SectionId::ResourceMeta) {
            Some(SectionData::ResourceMeta(meta)) => meta,
            _ => return Err(BytecodeError::MissingSection("RESOURCE_META".into())),
        };
        let io_map = match self.section(SectionId::IoMap) {
            Some(SectionData::IoMap(map)) => map,
            _ => return Err(BytecodeError::MissingSection("IO_MAP".into())),
        };

        validate_string_table(strings)?;
        if let Some(table) = debug_strings {
            validate_string_table(table)?;
        }
        validate_type_table(strings, types)?;
        validate_const_pool(strings, types, const_pool)?;
        validate_ref_table(strings, ref_table)?;
        validate_pou_index(strings, types, const_pool, pou_index, pou_bodies)?;
        validate_resource_meta(strings, ref_table, pou_index, resource_meta)?;
        validate_io_map(strings, types, ref_table, io_map)?;
        if let Some(SectionData::VarMeta(meta)) = self.section(SectionId::VarMeta) {
            validate_var_meta(strings, types, const_pool, ref_table, meta)?;
        }
        if let Some(SectionData::RetainInit(retain)) = self.section(SectionId::RetainInit) {
            validate_retain_init(const_pool, ref_table, retain)?;
        }
        if let Some(SectionData::DebugMap(debug_map)) = self.section(SectionId::DebugMap) {
            if self.version.minor >= 1 && debug_strings.is_none() {
                return Err(BytecodeError::MissingSection("DEBUG_STRING_TABLE".into()));
            }
            let file_strings = debug_strings.unwrap_or(strings);
            validate_debug_map(file_strings, pou_index, debug_map)?;
        }
        Ok(())
    }
}

fn validate_string_table(_strings: &StringTable) -> Result<(), BytecodeError> {
    Ok(())
}

fn validate_type_table(strings: &StringTable, types: &TypeTable) -> Result<(), BytecodeError> {
    for entry in &types.entries {
        if let Some(name_idx) = entry.name_idx {
            ensure_string_index(strings, name_idx)?;
        }
        match &entry.data {
            TypeData::Array { elem_type_id, dims } => {
                ensure_type_index(types, *elem_type_id)?;
                for (lower, upper) in dims {
                    if lower > upper {
                        return Err(BytecodeError::InvalidSection("invalid array bounds".into()));
                    }
                }
            }
            TypeData::Struct { fields } | TypeData::Union { fields } => {
                for field in fields {
                    ensure_string_index(strings, field.name_idx)?;
                    ensure_type_index(types, field.type_id)?;
                }
            }
            TypeData::Enum {
                base_type_id,
                variants,
            } => {
                ensure_type_index(types, *base_type_id)?;
                for variant in variants {
                    ensure_string_index(strings, variant.name_idx)?;
                }
            }
            TypeData::Alias { target_type_id }
            | TypeData::Subrange {
                base_type_id: target_type_id,
                ..
            }
            | TypeData::Reference { target_type_id } => {
                ensure_type_index(types, *target_type_id)?;
            }
            TypeData::Pou { .. } => {}
            TypeData::Interface { methods } => {
                for method in methods {
                    ensure_string_index(strings, method.name_idx)?;
                }
            }
            TypeData::Primitive { .. } => {}
        }
    }
    Ok(())
}

fn validate_const_pool(
    strings: &StringTable,
    types: &TypeTable,
    pool: &ConstPool,
) -> Result<(), BytecodeError> {
    for entry in &pool.entries {
        validate_const_payload(strings, types, entry)?;
    }
    Ok(())
}

fn validate_const_payload(
    strings: &StringTable,
    types: &TypeTable,
    entry: &ConstEntry,
) -> Result<(), BytecodeError> {
    let type_id = entry.type_id;
    let payload = &entry.payload;
    let entry = types
        .entries
        .get(type_id as usize)
        .ok_or_else(|| BytecodeError::InvalidIndex {
            kind: "type".into(),
            index: type_id,
        })?;
    let mut reader = BytecodeReader::new(payload);
    validate_const_payload_entry(strings, types, entry, &mut reader)?;
    if reader.remaining() != 0 {
        return Err(BytecodeError::InvalidSection("const payload length".into()));
    }
    Ok(())
}

fn validate_const_payload_entry(
    strings: &StringTable,
    types: &TypeTable,
    entry: &TypeEntry,
    reader: &mut BytecodeReader<'_>,
) -> Result<(), BytecodeError> {
    match &entry.data {
        TypeData::Primitive { prim_id, .. } => match prim_id {
            1 => {
                reader.read_u8()?;
            }
            2 | 6 | 10 | 26 => {
                reader.read_u8()?;
            }
            3 | 7 | 11 | 27 => {
                reader.read_u16()?;
            }
            4 | 8 | 12 => {
                reader.read_u32()?;
            }
            5 | 9 | 13 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 => {
                reader.read_u64()?;
            }
            14 => {
                reader.read_u32()?;
            }
            24 | 25 => {
                let idx = reader.read_u32()?;
                ensure_string_index(strings, idx)?;
            }
            _ => {
                return Err(BytecodeError::InvalidSection("unknown primitive".into()));
            }
        },
        TypeData::Array { elem_type_id, .. } => {
            let count = reader.read_u32()? as usize;
            let elem = types.entries.get(*elem_type_id as usize).ok_or_else(|| {
                BytecodeError::InvalidIndex {
                    kind: "type".into(),
                    index: *elem_type_id,
                }
            })?;
            for _ in 0..count {
                validate_const_payload_entry(strings, types, elem, reader)?;
            }
        }
        TypeData::Struct { fields } | TypeData::Union { fields } => {
            let count = reader.read_u32()? as usize;
            if count != fields.len() {
                return Err(BytecodeError::InvalidSection(
                    "struct/union constant count mismatch".into(),
                ));
            }
            for field in fields {
                let field_type = types.entries.get(field.type_id as usize).ok_or_else(|| {
                    BytecodeError::InvalidIndex {
                        kind: "type".into(),
                        index: field.type_id,
                    }
                })?;
                validate_const_payload_entry(strings, types, field_type, reader)?;
            }
        }
        TypeData::Enum { .. } => {
            reader.read_i64()?;
        }
        TypeData::Alias { target_type_id } => {
            let target = types.entries.get(*target_type_id as usize).ok_or_else(|| {
                BytecodeError::InvalidIndex {
                    kind: "type".into(),
                    index: *target_type_id,
                }
            })?;
            validate_const_payload_entry(strings, types, target, reader)?;
        }
        TypeData::Subrange { base_type_id, .. } => {
            let base = types.entries.get(*base_type_id as usize).ok_or_else(|| {
                BytecodeError::InvalidIndex {
                    kind: "type".into(),
                    index: *base_type_id,
                }
            })?;
            validate_const_payload_entry(strings, types, base, reader)?;
        }
        TypeData::Reference { .. } => {
            reader.read_u32()?;
        }
        _ => {
            return Err(BytecodeError::InvalidSection(
                "unsupported const type".into(),
            ));
        }
    }
    Ok(())
}

fn validate_ref_table(strings: &StringTable, table: &RefTable) -> Result<(), BytecodeError> {
    for entry in &table.entries {
        for segment in &entry.segments {
            if let RefSegment::Field { name_idx } = segment {
                ensure_string_index(strings, *name_idx)?;
            }
        }
    }
    Ok(())
}

fn validate_pou_index(
    strings: &StringTable,
    types: &TypeTable,
    const_pool: &ConstPool,
    index: &PouIndex,
    bodies: &[u8],
) -> Result<(), BytecodeError> {
    for entry in &index.entries {
        ensure_string_index(strings, entry.name_idx)?;
        if let Some(return_type_id) = entry.return_type_id {
            ensure_type_index(types, return_type_id)?;
        }
        if let Some(owner) = entry.owner_pou_id {
            if !index.entries.iter().any(|pou| pou.id == owner) {
                return Err(BytecodeError::InvalidPouId(owner));
            }
        }
        for param in &entry.params {
            ensure_string_index(strings, param.name_idx)?;
            ensure_type_index(types, param.type_id)?;
            if let Some(default_idx) = param.default_const_idx {
                ensure_const_index(const_pool, default_idx)?;
            }
        }
        if let Some(meta) = &entry.class_meta {
            if let Some(parent) = meta.parent_pou_id {
                if !index.entries.iter().any(|pou| pou.id == parent) {
                    return Err(BytecodeError::InvalidPouId(parent));
                }
            }
            for interface in &meta.interfaces {
                ensure_type_index(types, interface.interface_type_id)?;
                let interface_entry = types
                    .entries
                    .get(interface.interface_type_id as usize)
                    .ok_or_else(|| BytecodeError::InvalidIndex {
                        kind: "type".into(),
                        index: interface.interface_type_id,
                    })?;
                if !matches!(interface_entry.kind, TypeKind::Interface) {
                    return Err(BytecodeError::InvalidSection(
                        "interface mapping expects interface type".into(),
                    ));
                }
                if let TypeData::Interface { methods } = &interface_entry.data {
                    if interface.vtable_slots.len() != methods.len() {
                        return Err(BytecodeError::InvalidSection(
                            "interface mapping slot mismatch".into(),
                        ));
                    }
                }
            }
            for method in &meta.methods {
                ensure_string_index(strings, method.name_idx)?;
                if !index.entries.iter().any(|pou| pou.id == method.pou_id) {
                    return Err(BytecodeError::InvalidPouId(method.pou_id));
                }
            }
        }
        let start = entry.code_offset as usize;
        let end = start + entry.code_length as usize;
        if end > bodies.len() {
            return Err(BytecodeError::InvalidSection(
                "POU code out of bounds".into(),
            ));
        }
        validate_instruction_stream(index, types, start, &bodies[start..end])?;
    }
    Ok(())
}

fn validate_instruction_stream(
    index: &PouIndex,
    types: &TypeTable,
    _base: usize,
    code: &[u8],
) -> Result<(), BytecodeError> {
    let mut reader = BytecodeReader::new(code);
    let mut starts = Vec::new();
    let mut jumps = Vec::new();
    while reader.remaining() > 0 {
        let pc = reader.pos();
        starts.push(pc as i32);
        let opcode = reader.read_u8()?;
        match opcode {
            0x00 | 0x01 | 0x06 | 0x11 | 0x12 | 0x13 | 0x14 | 0x15 | 0x31 | 0x32 | 0x33 | 0x40
            | 0x41 | 0x42 | 0x43 | 0x44 | 0x45 | 0x46 | 0x47 | 0x48 | 0x49 | 0x4A | 0x4B | 0x4C
            | 0x4D | 0x4E | 0x50 | 0x51 | 0x52 | 0x53 | 0x54 | 0x55 => {}
            0x02..=0x04 => {
                let offset = reader.read_i32()?;
                jumps.push((pc as i32, offset));
            }
            0x05 => {
                let pou_id = reader.read_u32()?;
                if !index.entries.iter().any(|pou| pou.id == pou_id) {
                    return Err(BytecodeError::InvalidPouId(pou_id));
                }
            }
            0x07 => {
                reader.read_u32()?; // vtable slot
            }
            0x08 => {
                let interface_type_id = reader.read_u32()?;
                let slot = reader.read_u32()?;
                let entry = types
                    .entries
                    .get(interface_type_id as usize)
                    .ok_or_else(|| BytecodeError::InvalidIndex {
                        kind: "type".into(),
                        index: interface_type_id,
                    })?;
                if !matches!(entry.kind, TypeKind::Interface) {
                    return Err(BytecodeError::InvalidSection(
                        "CALL_VIRTUAL expects interface type".into(),
                    ));
                }
                if let TypeData::Interface { methods } = &entry.data {
                    if slot as usize >= methods.len() {
                        return Err(BytecodeError::InvalidSection(
                            "CALL_VIRTUAL slot out of range".into(),
                        ));
                    }
                }
            }
            0x10 => {
                reader.read_u32()?;
            }
            0x16 => {
                reader.read_u8()?;
            }
            0x20..=0x22 => {
                reader.read_u32()?;
            }
            0x23 => {}
            0x30 => {
                reader.read_u32()?;
            }
            0x60 => {
                let type_id = reader.read_u32()?;
                ensure_type_index(types, type_id)?;
            }
            0x70 => {
                reader.read_u32()?;
            }
            _ => return Err(BytecodeError::InvalidOpcode(opcode)),
        }
    }
    let code_len = code.len() as i32;
    let start_set: HashSet<i32> = starts.into_iter().collect();
    for (pc, offset) in jumps {
        let target = pc + 1 + 4 + offset;
        if target < 0 || target > code_len {
            return Err(BytecodeError::InvalidJumpTarget(target));
        }
        if target != code_len && !start_set.contains(&target) {
            return Err(BytecodeError::InvalidJumpTarget(target));
        }
    }
    Ok(())
}

fn validate_resource_meta(
    strings: &StringTable,
    ref_table: &RefTable,
    pou_index: &PouIndex,
    meta: &ResourceMeta,
) -> Result<(), BytecodeError> {
    let mut program_names = HashSet::new();
    for entry in &pou_index.entries {
        if entry.kind == PouKind::Program {
            let name = strings
                .entries
                .get(entry.name_idx as usize)
                .ok_or_else(|| BytecodeError::InvalidIndex {
                    kind: "string".into(),
                    index: entry.name_idx,
                })?;
            program_names.insert(name.to_ascii_uppercase());
        }
    }

    for resource in &meta.resources {
        ensure_string_index(strings, resource.name_idx)?;
        for task in &resource.tasks {
            ensure_string_index(strings, task.name_idx)?;
            if let Some(single_idx) = task.single_name_idx {
                ensure_string_index(strings, single_idx)?;
            }
            for idx in &task.program_name_idx {
                ensure_string_index(strings, *idx)?;
                let name = strings.entries.get(*idx as usize).ok_or_else(|| {
                    BytecodeError::InvalidIndex {
                        kind: "string".into(),
                        index: *idx,
                    }
                })?;
                if !program_names.contains(&name.to_ascii_uppercase()) {
                    return Err(BytecodeError::InvalidSection(
                        format!("task references unknown program '{}'", name).into(),
                    ));
                }
            }
            for idx in &task.fb_ref_idx {
                if *idx as usize >= ref_table.entries.len() {
                    return Err(BytecodeError::InvalidIndex {
                        kind: "ref".into(),
                        index: *idx,
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_io_map(
    strings: &StringTable,
    types: &TypeTable,
    ref_table: &RefTable,
    map: &IoMap,
) -> Result<(), BytecodeError> {
    for binding in &map.bindings {
        ensure_string_index(strings, binding.address_str_idx)?;
        if binding.ref_idx as usize >= ref_table.entries.len() {
            return Err(BytecodeError::InvalidIndex {
                kind: "ref".into(),
                index: binding.ref_idx,
            });
        }
        if let Some(type_id) = binding.type_id {
            ensure_type_index(types, type_id)?;
        }
    }
    Ok(())
}

fn validate_var_meta(
    strings: &StringTable,
    types: &TypeTable,
    const_pool: &ConstPool,
    ref_table: &RefTable,
    meta: &VarMeta,
) -> Result<(), BytecodeError> {
    for entry in &meta.entries {
        ensure_string_index(strings, entry.name_idx)?;
        ensure_type_index(types, entry.type_id)?;
        ensure_ref_index(ref_table, entry.ref_idx)?;
        if entry.retain > 3 {
            return Err(BytecodeError::InvalidSection(
                "invalid retain policy".into(),
            ));
        }
        if let Some(init_idx) = entry.init_const_idx {
            ensure_const_index(const_pool, init_idx)?;
        }
    }
    Ok(())
}

fn validate_retain_init(
    const_pool: &ConstPool,
    ref_table: &RefTable,
    retain: &RetainInit,
) -> Result<(), BytecodeError> {
    for entry in &retain.entries {
        ensure_ref_index(ref_table, entry.ref_idx)?;
        ensure_const_index(const_pool, entry.const_idx)?;
    }
    Ok(())
}

fn validate_debug_map(
    strings: &StringTable,
    pou_index: &PouIndex,
    map: &DebugMap,
) -> Result<(), BytecodeError> {
    for entry in &map.entries {
        let pou = pou_index
            .entries
            .iter()
            .find(|pou| pou.id == entry.pou_id)
            .ok_or(BytecodeError::InvalidPouId(entry.pou_id))?;
        let end = pou
            .code_offset
            .checked_add(pou.code_length)
            .ok_or_else(|| BytecodeError::InvalidSection("POU code range overflow".into()))?;
        if entry.code_offset < pou.code_offset || entry.code_offset > end {
            return Err(BytecodeError::InvalidSection(
                "debug map code offset out of bounds".into(),
            ));
        }
        ensure_string_index(strings, entry.file_idx)?;
    }
    Ok(())
}

fn ensure_string_index(strings: &StringTable, idx: u32) -> Result<(), BytecodeError> {
    if idx as usize >= strings.entries.len() {
        return Err(BytecodeError::InvalidIndex {
            kind: "string".into(),
            index: idx,
        });
    }
    Ok(())
}

fn ensure_type_index(types: &TypeTable, idx: u32) -> Result<(), BytecodeError> {
    if idx as usize >= types.entries.len() {
        return Err(BytecodeError::InvalidIndex {
            kind: "type".into(),
            index: idx,
        });
    }
    Ok(())
}

fn ensure_const_index(pool: &ConstPool, idx: u32) -> Result<(), BytecodeError> {
    if idx as usize >= pool.entries.len() {
        return Err(BytecodeError::InvalidIndex {
            kind: "const".into(),
            index: idx,
        });
    }
    Ok(())
}

fn ensure_ref_index(table: &RefTable, idx: u32) -> Result<(), BytecodeError> {
    if idx as usize >= table.entries.len() {
        return Err(BytecodeError::InvalidIndex {
            kind: "ref".into(),
            index: idx,
        });
    }
    Ok(())
}
