#![no_std]
pub mod consts;
pub mod soft;

#[unsafe(no_mangle)]
pub extern "C" fn compress256(state: &mut [u32; 8], block: &[u8; 64]) {
    soft::compress(state, core::slice::from_ref(block));
}
