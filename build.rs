fn main() {
    glib_build_tools::compile_resources(&["assets"], "assets/fgdb.gresource.xml", "fgdb.gresource");

    println!("cargo:rerun-if-changed=src/syscalls/count.bpf.c");
    println!("cargo:rerun-if-env-changed=BPF_CLANG");
    println!("cargo:rustc-check-cfg=cfg(syscall_bpf)");
    let architecture = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    if !matches!(architecture.as_str(), "x86_64" | "aarch64") {
        return;
    }

    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let endian = std::env::var("CARGO_CFG_TARGET_ENDIAN").unwrap();
    let compiler = std::env::var_os("BPF_CLANG").unwrap_or_else(|| "clang".into());
    let temporary = output.join(format!("syscalls-{}.bpf.o", std::process::id()));

    let status = std::process::Command::new(compiler)
        .args([
            "-target",
            if endian == "little" { "bpfel" } else { "bpfeb" },
        ])
        .args(["-O2", "-g", "-Wall", "-Werror", "-c"])
        .arg(format!("-DFGDB_{architecture}"))
        .arg("src/syscalls/count.bpf.c")
        .arg("-o")
        .arg(&temporary)
        .status()
        .expect("syscall collection requires Clang with the BPF target (or set BPF_CLANG)");

    if !status.success() {
        let _ = std::fs::remove_file(&temporary);
        panic!("failed to compile the syscall counter");
    }

    std::fs::rename(temporary, output.join("syscalls.bpf.o"))
        .expect("failed to install the compiled syscall counter");

    println!("cargo:rustc-cfg=syscall_bpf");
}
