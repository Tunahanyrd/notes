//! Plain C-ABI exports for `wasm32-unknown-unknown` (no wasm-bindgen).
//!
//! JS protocol (desktop/src/inkLive.ts):
//! 1. `ink_alloc(len)` → write UTF-8 pen JSON → `ink_pen_new(ptr, len)` → pen id
//!    (0 = refused; `ink_error` describes why).
//! 2. `ink_alloc(n * 32)` → write `n` samples as little-endian f64 quadruples
//!    `[x, y, pressure, timeMs]` → `ink_render(pen, ptr, n, scale)` → 1 on
//!    success, 0 on refusal.
//! 3. Read `ink_out_rect()` (four i32: x, y, width, height in device pixels)
//!    and `ink_out_ptr()`/`ink_out_len()` (straight-alpha RGBA).
//! 4. `ink_free(ptr, len)`; `ink_pen_drop(pen)` when the pen is no longer used.
//!
//! The module is single-threaded; the output buffer is reused by the next call.
use crate::{Pen, Sample};
use std::cell::RefCell;
use std::collections::HashMap;

struct State {
    pens: HashMap<u32, Pen>,
    next: u32,
    out: Vec<u8>,
    rect: [i32; 4],
    error: &'static str,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State { pens: HashMap::new(), next: 1, out: Vec::new(), rect: [0; 4], error: "" });
}

/// 8-byte aligned buffer (sample arrays are read as f64).
#[unsafe(no_mangle)]
pub extern "C" fn ink_alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u64>::with_capacity(len.div_ceil(8).max(1));
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr as *mut u8
}

/// # Safety
/// `ptr` must come from `ink_alloc(len)` with the same `len`, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ink_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(unsafe { Vec::from_raw_parts(ptr as *mut u64, 0, len.div_ceil(8).max(1)) });
    }
}

/// # Safety
/// `ptr..ptr+len` must be readable memory written by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ink_pen_new(ptr: *const u8, len: usize) -> u32 {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let parsed = std::str::from_utf8(bytes).map_err(|_| crate::Error("invalid pen")).and_then(Pen::from_json);
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        match parsed {
            Ok(pen) => {
                let id = state.next;
                state.next = state.next.wrapping_add(1).max(1);
                state.pens.insert(id, pen);
                id
            }
            Err(e) => {
                state.error = e.0;
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn ink_pen_drop(id: u32) {
    STATE.with(|cell| {
        cell.borrow_mut().pens.remove(&id);
    });
}

/// # Safety
/// `ptr` must point at `count * 4` little-endian f64 values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ink_render(id: u32, ptr: *const f64, count: usize, scale: f64) -> u32 {
    if count > crate::MAX_POINTS {
        return STATE.with(|cell| {
            cell.borrow_mut().error = "capture channel count bound";
            0
        });
    }
    let values = unsafe { std::slice::from_raw_parts(ptr, count * 4) };
    let samples: Vec<Sample> = values
        .chunks_exact(4)
        .map(|v| Sample { x: v[0], y: v[1], pressure: v[2], time_ms: v[3] as i64 })
        .collect();
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        let result = match state.pens.get(&id) {
            None => Err(crate::Error("unknown pen")),
            Some(pen) => pen.raster(&samples, scale),
        };
        match result {
            Ok(raster) => {
                state.rect = [raster.x, raster.y, raster.width as i32, raster.height as i32];
                state.out = raster.rgba;
                1
            }
            Err(e) => {
                state.error = e.0;
                state.rect = [0; 4];
                state.out.clear();
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn ink_out_ptr() -> *const u8 {
    STATE.with(|cell| cell.borrow().out.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn ink_out_len() -> usize {
    STATE.with(|cell| cell.borrow().out.len())
}

#[unsafe(no_mangle)]
pub extern "C" fn ink_out_rect() -> *const i32 {
    STATE.with(|cell| cell.borrow().rect.as_ptr())
}

/// Last refusal reason as a static NUL-terminated ASCII string.
#[unsafe(no_mangle)]
pub extern "C" fn ink_error() -> *const u8 {
    let reason = STATE.with(|cell| cell.borrow().error);
    // Static reasons have no NUL; return a pointer into a leaked copy once per reason.
    thread_local! { static CACHE: RefCell<HashMap<&'static str, Vec<u8>>> = RefCell::new(HashMap::new()); }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.entry(reason).or_insert_with(|| {
            let mut v = reason.as_bytes().to_vec();
            v.push(0);
            v
        }).as_ptr()
    })
}
