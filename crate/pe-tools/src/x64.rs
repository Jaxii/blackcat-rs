#[allow(non_snake_case)]
use crate::shared::*;

use std::collections::HashMap;
use std::mem::size_of;
use std::ptr::null_mut;

use anyhow::*;
use goblin::pe::{
    PE,
    optional_header::OptionalHeader,
    section_table::SectionTable
};
use ntapi::{
    ntldr::LDR_DATA_TABLE_ENTRY,
    ntpebteb::PEB,
    winapi_local::um::winnt::__readgsqword
};
use winapi::{
    ctypes::c_void,
    shared::{
        basetsd::DWORD64,
        ntdef::{LPCSTR, PVOID},
    },
    um::{
        memoryapi::{ReadProcessMemory, WriteProcessMemory},
        winnt::{
            IMAGE_ORDINAL64, IMAGE_IMPORT_BY_NAME, IMAGE_IMPORT_DESCRIPTOR,
            IMAGE_DOS_HEADER, IMAGE_NT_HEADERS64,
            IMAGE_SNAP_BY_ORDINAL64, IMAGE_THUNK_DATA64,
            IMAGE_TLS_DIRECTORY64, DLL_PROCESS_ATTACH
        },
    }
};

pub struct PEContainer<'a> {
    pub pe: PE<'a>,
    first_pointer: PVOID,
    old_image_base_address: PVOID
}

impl PEContainer<'_> {
    pub fn new<'a>(image_base_address: PVOID, is_memory: bool) -> Result<PEContainer<'a>> {
        Ok(PEContainer {
            pe: if is_memory {
                let opts = goblin::pe::options::ParseOptions{ resolve_rva: false };
                goblin::pe::PE::parse_with_opts(ptr_to_u8slice(image_base_address), &opts)?
            } else {
                goblin::pe::PE::parse(ptr_to_u8slice(image_base_address))?
            },
            first_pointer: image_base_address,
            old_image_base_address: 0x0 as _,
        })
    }

    pub fn new_from_u8<'a>(buffer: &'a [u8], is_memory: bool) -> Result<PEContainer<'a>> {
        Ok(PEContainer {
            pe: if is_memory {
                let opts = goblin::pe::options::ParseOptions{ resolve_rva: false };
                goblin::pe::PE::parse_with_opts(buffer, &opts)?
            } else {
                goblin::pe::PE::parse(buffer)?
            },
            first_pointer: &buffer[0] as *const _ as PVOID,
            old_image_base_address: 0x0 as _,
        })
    }

    pub fn first_pointer(&self) -> PVOID {
        self.first_pointer
    }

    pub fn image_base_address(&self) -> PVOID {
        match self.pe.header.optional_header {
            Some(optional_header) => {
                optional_header.windows_fields.image_base as _
            },
            None => { 0x0 as _ }
        }
    }

    pub fn image_size(&self) -> u32 {
        self.get_optional_headers().windows_fields.size_of_image
    }

    pub fn change_image_base_address(&mut self, new_image_base_address: PVOID, is_memory: bool) -> Result<()> {
        unsafe {
            let dos_header = self.first_pointer as *mut IMAGE_DOS_HEADER;
            let nt_header = (self.first_pointer as usize + (*dos_header).e_lfanew as usize) as *mut IMAGE_NT_HEADERS64;
            self.old_image_base_address = (*nt_header).OptionalHeader.ImageBase as _;
            (*nt_header).OptionalHeader.ImageBase = new_image_base_address as _;
        };
        self.pe = if is_memory {
            let opts = goblin::pe::options::ParseOptions{ resolve_rva: false };
            goblin::pe::PE::parse_with_opts(ptr_to_u8slice(self.first_pointer), &opts)?
        } else {
            goblin::pe::PE::parse(ptr_to_u8slice(self.first_pointer))?
        };
        Ok(())
    }

    pub fn to_va(&self, rva: u64) -> PVOID {
        (self.image_base_address() as u64 + rva) as PVOID
    }

    pub fn get_optional_headers(&self) -> OptionalHeader {
        self.pe.header.optional_header.unwrap()
    }

    pub fn get_image_size(&self) -> u32 {
        self.pe.header.optional_header.unwrap().windows_fields.size_of_image
    }

    pub fn get_section_headers(&self) -> &Vec<SectionTable> {
        &self.pe.sections
    }

    pub fn is_relocatable(&self) -> bool {
        self.get_optional_headers().data_directories.get_base_relocation_table().is_some()
    }

    pub fn copy_headers_to(&self, target_image_base_address: PVOID) -> Result<()> {
        unsafe {
            for i in 0..self.get_optional_headers().windows_fields.size_of_headers {
                *((target_image_base_address as u64 + i as u64) as *mut u8) = *((self.image_base_address() as u64 + i as u64) as *mut u8)
            }
            Ok(())
        }
    }

    pub fn remote_copy_headers_to(&self, hp: PVOID, target_image_base_address: PVOID) -> Result<()> {
        if unsafe {
            WriteProcessMemory(
                hp,
                target_image_base_address as _,
                self.first_pointer(),
                self.get_optional_headers().windows_fields.size_of_headers as usize,
                null_mut(),
            )
        } == 0
        {
            bail!("could not write process memory.");
        }
        Ok(())
    }

    pub fn copy_section_headers_to(&self, target_image_base_address: PVOID) -> Result<()> {
        unsafe {
            for section in self.get_section_headers() {
                let p_dest_section = target_image_base_address as u64 + section.virtual_address as u64;
                for i in 0..section.size_of_raw_data {
                    *((p_dest_section + i as u64) as *mut u8) =
                        *((self.image_base_address() as u64 + section.pointer_to_raw_data as u64 + i as u64) as *mut u8);
                }
            }
            Ok(())
        }
    }

    pub fn remote_copy_section_headers_to(&self, hp: PVOID, target_image_base_address: PVOID) -> Result<()> {
        for section in self.get_section_headers() {
            let p_dest_section = target_image_base_address as u64 + section.virtual_address as u64;
            if unsafe {
                WriteProcessMemory(
                    hp,
                    p_dest_section as *mut _,
                    (self.first_pointer() as u64 + section.pointer_to_raw_data as u64)
                        as *const c_void as *mut _,
                    section.size_of_raw_data as usize,
                    null_mut(),
                ) == 0
            } {
                bail!("could not write process memory.");
            }
        }

        Ok(())
    }

    pub fn resolve_import(&self, target_image_base_address: PVOID, p_load_liberay: PLoadLibraryA, p_get_proc_adress: PGetProcAddress) -> Result<()> {
        unsafe {
            let import_directory = self.get_optional_headers().data_directories.get_import_table().unwrap();
            let mut import_discriptor = (target_image_base_address as u64
                + import_directory.virtual_address as u64)
                as *mut IMAGE_IMPORT_DESCRIPTOR;

            while (*import_discriptor).Name != 0x0 {
                let lib_name = (target_image_base_address as u32 + (*import_discriptor).Name) as LPCSTR;
                let lib = p_load_liberay(lib_name);

                let mut orig_thunk = (target_image_base_address as u64
                    + *(*import_discriptor).u.OriginalFirstThunk() as u64)
                    as *mut IMAGE_THUNK_DATA64;
                let mut thunk = (target_image_base_address as u64 + (*import_discriptor).FirstThunk as u64)
                    as *mut IMAGE_THUNK_DATA64;

                while (*thunk).u1.AddressOfData() != &0x0 {
                    if orig_thunk != null_mut() && IMAGE_SNAP_BY_ORDINAL64(*(*orig_thunk).u1.Ordinal()) {
                        let fn_ordinal = IMAGE_ORDINAL64(*(*orig_thunk).u1.Ordinal()) as LPCSTR;
                        *(*thunk).u1.Function_mut() = p_get_proc_adress(lib, fn_ordinal) as _;
                    } else {
                        let fn_name = (target_image_base_address as u64 + *(*thunk).u1.AddressOfData()) as *mut IMAGE_IMPORT_BY_NAME;
                        *(*thunk).u1.Function_mut() = p_get_proc_adress(lib, (*fn_name).Name[0] as _) as _;
                    }

                    thunk = (thunk as usize + size_of::<DWORD64>()) as _;
                    if orig_thunk != null_mut() {
                        orig_thunk = (orig_thunk as usize + size_of::<DWORD64>()) as _;
                    }
                }

                import_discriptor = (import_discriptor as usize + size_of::<DWORD64>()) as _;
            }

            Ok(())
        }
    }

    pub fn exec_tls_callback(&self, target_image_base_address: PVOID) -> Result<()> {
        unsafe { 
            let tls_table = self.get_optional_headers().data_directories.get_tls_table().unwrap();
            let tls_directry = std::slice::from_raw_parts::<PVOID>(self.to_va(tls_table.virtual_address as _) as _, tls_table.size as _);

            for tls in tls_directry {
                let callback = (*(*(tls) as *mut IMAGE_TLS_DIRECTORY64)).AddressOfCallBacks;
                std::mem::transmute::<*const c_void, TLS_CALLBACK>(*(callback as *const *const c_void))(
                    target_image_base_address,
                    DLL_PROCESS_ATTACH,
                    0 as _,
                );
            }
            Ok(())
        }
    }

    pub fn search_expoted_func(&self, function_name: impl Into<String>) -> anyhow::Result<*mut c_void> {
        if !self.pe.is_lib { bail!("this PE is not dll."); };

        let function_name = function_name.into();

        for e in &self.pe.exports {
            match e.name {
                Some(symbol) => {
                    if symbol == function_name {
                        return Ok((self.image_base_address() as usize + e.offset) as _);
                    }
                },
                None => {}
            }
        };

        bail!("could not find {}", function_name);
    }

    pub fn delta_relocation(
        &self,
        target_image_base_address: PVOID
    ) -> Result<()> {
        Ok(unsafe {
            self.delta_relocation_closure(target_image_base_address, |target, delta| {
                if delta.is_minus {
                    *(target as *mut u64) = *(target as *mut u64) - delta.offset as u64;
                } else {
                    *(target as *mut u64) = *(target as *mut u64) + delta.offset as u64;
                }
                Ok(())
            })?;
        })
    }

    pub fn remote_delta_relocation(
        &self,
        hp: PVOID,
        target_image_base_address: PVOID
    ) -> Result<()> {
        Ok(unsafe {
            self.delta_relocation_closure(target_image_base_address, |target, delta| {
                let mut d_buffer = 0 as u64;

                if ReadProcessMemory(
                    hp,
                    (target) as PVOID,
                    &mut d_buffer as *const _ as *mut _,
                    size_of::<u64>(),
                    null_mut(),
                ) == 0
                {
                    bail!("could not read memory from new dest image.")
                }

                d_buffer = if delta.is_minus {
                    d_buffer - delta.offset as u64
                } else {
                    d_buffer + delta.offset as u64
                };

                if WriteProcessMemory(
                    hp,
                    (target) as PVOID,
                    &mut d_buffer as *const _ as *mut _,
                    size_of::<u64>(),
                    null_mut(),
                ) == 0
                {
                    bail!("could not write memory to new dest image.")
                }

                Ok(())
            })?;
        })
    }

    fn delta_relocation_closure<T: Fn(u64, &Delta) -> Result<()>>(
        &self,
        target_image_base_address: PVOID,
        process_fn: T
    ) -> Result<()> {
        unsafe {
            let image_base_address = if self.old_image_base_address == 0x0 as _ {
                self.image_base_address()
            } else {
                self.old_image_base_address
            };

            let delta = Delta::calculate_delta(target_image_base_address as _, image_base_address as _);

            if !self.is_relocatable() || delta.offset == 0 { return Ok(()) }

            let block_buffer = self.first_pointer();

            for section in self.get_section_headers() {
                if section.name != DOT_RELOC { continue }

                let reloc_address = section.pointer_to_raw_data as u64;
                let mut offset = 0 as u64;
                let reloc_data = self.get_optional_headers().data_directories.get_base_relocation_table().unwrap();

                while offset < reloc_data.size as u64 {
                    let block_header = std::ptr::read::<BASE_RELOCATION_BLOCK>(
                        (block_buffer as usize + (reloc_address + offset) as usize)
                            as *const _,
                    );

                    offset = offset + std::mem::size_of::<BASE_RELOCATION_BLOCK>() as u64;

                    // 2 is relocation entry size.
                    // ref: https://docs.microsoft.com/en-us/windows/win32/debug/pe-format#base-relocation-types
                    let entry_count = (block_header.BlockSize
                        - std::mem::size_of::<BASE_RELOCATION_BLOCK>() as u32)
                        / 2;

                    let block_entry = std::slice::from_raw_parts::<[u8; 2]>(
                        (block_buffer as usize + (reloc_address + offset) as usize)
                            as *const _,
                        entry_count as usize,
                    );

                    for block in block_entry {
                        let block = BASE_RELOCATION_ENTRY(*block);

                        offset = offset + 2;

                        if block.block_type() == 0 {
                            continue;
                        }

                        process_fn(target_image_base_address as u64 + block_header.PageAddress as u64 + block.offset() as u64, &delta)?;
                    }
                }
            }
        }

        Ok(())
    }
}

pub fn search_proc_address_from_loaded_module(function_name: impl Into<String>) -> anyhow::Result<*mut c_void> {
    unsafe {
        let function_name = function_name.into();
        let ppeb = __readgsqword(0x60) as *mut PEB;

        let p_peb_ldr_data = (*ppeb).Ldr;
        let mut module_list = (*p_peb_ldr_data).InLoadOrderModuleList.Flink as *mut LDR_DATA_TABLE_ENTRY;

        while !(*module_list).DllBase.is_null() {
            let dll_base = (*module_list).DllBase;
            let size = (*module_list).SizeOfImage as usize;

            module_list = (*module_list).InLoadOrderLinks.Flink as *mut LDR_DATA_TABLE_ENTRY;

            let buffer = std::slice::from_raw_parts::<u8>(dll_base as _, size);

            let opts = goblin::pe::options::ParseOptions{ resolve_rva: false };
            let res = goblin::pe::PE::parse_with_opts(buffer, &opts);

            // temp solution
            if res.is_err() { continue }
            let parsed = res?;

            if !parsed.is_lib { continue }

            for e in parsed.exports {
                match e.name {
                    Some(symbol) => {
                        if symbol == function_name {
                            return Ok((dll_base as usize + e.offset) as _);
                        }
                    },
                    None => {}
                }
            }
        }

        bail!("could not find {}", function_name);
    }
}

pub fn get_syscall_table() -> anyhow::Result<HashMap<String, usize>> {
    unsafe {
        let mut function_table = HashMap::new();
        let ppeb = __readgsqword(0x60) as *mut PEB;

        let p_peb_ldr_data = (*ppeb).Ldr;
        let mut module_list = (*p_peb_ldr_data).InLoadOrderModuleList.Flink as *mut LDR_DATA_TABLE_ENTRY;

        while !(*module_list).DllBase.is_null() {
            let dll_base = (*module_list).DllBase;
            let size = (*module_list).SizeOfImage as usize;

            module_list = (*module_list).InLoadOrderLinks.Flink as *mut LDR_DATA_TABLE_ENTRY;

            let buffer = std::slice::from_raw_parts::<u8>(dll_base as _, size);

            let opts = goblin::pe::options::ParseOptions{ resolve_rva: false };
            let res = goblin::pe::PE::parse_with_opts(buffer, &opts);

            // temp solution
            //if res.is_err() { continue }
            let parsed = res?;

            if !parsed.is_lib || parsed.name.unwrap() != "ntdll.dll" { continue }

            for e in parsed.exports {
                match e.name {
                    Some(symbol) => {
                        if symbol.len() < 2 { continue }

                        let sym_0 = symbol.chars().nth(0).unwrap();
                        let sym_1 = symbol.chars().nth(1).unwrap();

                        if (sym_0 == 'Z') && sym_1 == 'w' {
                            let mut function_name = symbol.to_string();

                            if symbol.chars().nth(0).unwrap() != 'N' { function_name.replace_range(0..2, "Nt"); };

                            function_table.insert(function_name, e.offset);
                        }
                    },
                    None => {}
                }
            }

            let mut function_table_vec: Vec<(String, usize)> = function_table.into_iter().collect();
            function_table_vec.sort_by(|x, y| x.1.cmp(&y.1));

            let mut syscall_table = HashMap::new();

            for (i, val) in function_table_vec.into_iter().enumerate() {
                syscall_table.insert(val.0, i);
            }

            return Ok(syscall_table);
        }

        bail!("could not find ntdll");
    }
}
