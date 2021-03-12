pub mod pe;

// TODO: remove this module
pub mod mem;

use crate::pe::{ read_remote_image64, get_image_base_address, x96_check };
use anyhow::*;
use winapi::shared::ntstatus::STATUS_SUCCESS;
use winapi::um::{
    processthreadsapi::{ CreateProcessA, STARTUPINFOA, PROCESS_INFORMATION },
    winbase::CREATE_SUSPENDED,
};
use ntapi::ntmmapi:: NtUnmapViewOfSection;

use std::ffi::CString;
use std::mem::zeroed;
use std::ptr::null_mut;

pub unsafe fn hollow(src: impl Into<String>, dest: impl Into<String>) -> Result<()> {
    // Create dest process
    let mut startup = zeroed::<STARTUPINFOA>();
    let mut process_info = zeroed::<PROCESS_INFORMATION>();

    let dest = CString::new("notepad").expect("CString::new failed");
    CreateProcessA(null_mut(), dest.as_ptr() as *mut _, null_mut(), null_mut(), 0, CREATE_SUSPENDED, null_mut(), null_mut(), &mut startup, &mut process_info);

    // Get dest image, image_address
    let hp = process_info.hProcess;
    let image_address = get_image_base_address(hp);
    let image = read_remote_image64(hp, image_address)?;

    // TODO: remove debug print
    println!("Signature: {:?}", (*image.FileHeader).Signature);
    println!("Machine: {:?}", (*image.FileHeader).FileHeader.Machine);
    println!("Architecture: {:?}", x96_check(process_info.hProcess, image_address));

    // TODO: read src program and mapping
    if false { unimplemented!() }

    // TODO: Unmapping image from dest process
    if NtUnmapViewOfSection(hp, image_address as *mut _) != STATUS_SUCCESS { bail!("could not unmapping image from dest process.") };

    // TODO: Allocate memory for src program
    if false { unimplemented!() }

    // TODO: Delta relocation
    if false { unimplemented!() }

    // TODO: Get thread context of dest, and modify entry point
    if false { unimplemented!() }

    // TODO: Resume thread of dest
    if false { unimplemented!() }

    // TODO: remove debug print
    println!("process was hollowed ε٩(๑> 3 <)۶з");

    Ok(())
}
