// Hello World for megos + wasm
#![no_main]
#![no_std]

use core::fmt::Write;
use megoslib::*;

#[no_mangle]
fn _start() {
    println!("hello, world");
}
