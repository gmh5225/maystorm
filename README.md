# MEG-OS codename Maystorm

My hobby operating system written in Rust, which Supports multitasking, multi-window, WebAssembly and simple applications.

* [Documentation](https://meg-os.github.io/maystorm/kernel/)

## Feature

* A hobby OS written in Rust
* Not a POSIX clone system
* Support for WebAssembly

## Requirements

### UEFI PC Platform

* 64bit UEFI v2.X+
* ACPI v2.X+
* SMBIOS v2.X+ (optional)
* x64 processor with up to 64 cores
* ??? MB of system memory
* 800 x 600 pixel resolution

## Build Environment

* Rust nightly
* nasm
* llvm (ld.lld)
* qemu + ovmf (optional)

### To build

1. Install llvm
2. Install rust (nightly)
3. `make apps`
4. `make install`

If you get an error that the linker cannot be found, configure your linker in `~/.cargo/config.toml` or something similar.

```
[target.x86_64-unknown-none]
linker = "/opt/homebrew/opt/llvm/bin/ld.lld"
```

### To run on qemu

1. Copy qemu's OVMF for x64 to `var/ovmfx64.fd`.
2. Follow the build instructions to finish the installation.
3. `make run`

### To run on real hardware

* Copy the files in the path `mnt/efi` created by the build to a USB memory stick and reboot your computer.
* You may need to change settings such as SecureBoot.

## HOE: Haribote-OS Emulation Subsystem

* We have confirmed that about half of the apps work at this point. Some APIs are not yet implemented.
* This subsystem may not be supported in the future, or its architecture may change.

## History

### 2020-05-09

* Initial Commit

## LICENSE

MIT License

&copy; 2020 MEG-OS Project.

### Wall paper

* CC BY-SA 4.0 &copy; 猫(1010) 

## Contributors

### Kernel

[![Nerry](https://github.com/neri.png?size=50)](https://github.com/neri "Nerry")

### Wall paper

[![猫(1010)](https://github.com/No000.png?size=50)](https://github.com/No000 "猫(1010)")
