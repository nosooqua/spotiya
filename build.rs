fn main() {
    // Build for the same target as the receiver; include the bytes in the executable so
    // upgrades cannot leave an incompatible optional AirPlay helper behind.
    let tap = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("airplay-tap.so");
    let compiler = cc::Build::new().get_compiler();
    let result = compiler
        .to_command()
        .args([
            "-shared",
            "-fPIC",
            "-Os",
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "controls/airplay_tap.c",
            "-o",
        ])
        .arg(&tap)
        .args(["-ldl", "-pthread"])
        .status()
        .expect("compile AirPlay PCM observer");
    assert!(result.success(), "AirPlay PCM observer compilation failed");
    println!("cargo:rerun-if-changed=controls/airplay_tap.c");
    let arm = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("arm");
    let mut native = cc::Build::new();
    if arm {
        native.file("controls/alsa_compat.c");
    }
    native
        .file("controls/input.c")
        .file("controls/ducking.c")
        .file("controls/stdio.c")
        .flag_if_supported("-std=c11")
        .flag_if_supported("-Os")
        .flag_if_supported("-fstack-protector-strong")
        .warnings(true)
        .warnings_into_errors(true)
        .compile("mini2_controls");
    for name in ["lights.h", "display.h"] {
        println!("cargo:rerun-if-changed=controls/mini2/{name}");
    }
    println!("cargo:rerun-if-changed=controls/stdio.c");
    println!("cargo:rerun-if-changed=controls/mapped_input.h");
    println!("cargo:rerun-if-changed=controls/input.c");
    println!("cargo:rerun-if-changed=controls/ducking.c");
    println!("cargo:rerun-if-changed=controls/ducking.h");
    println!("cargo:rerun-if-changed=controls/input_config.h");
    println!("cargo:rerun-if-changed=controls/alsa_compat.c");
    if arm {
        // Vendor ALSA plugins also load libraries from this directory.
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags,-rpath,/system/vendor/quasar/libs");
        for name in [
            "get_buffer_size",
            "get_buffer_size_min",
            "get_buffer_size_max",
            "get_period_size",
            "get_period_size_min",
            "get_period_size_max",
            "set_buffer_size_near",
            "set_period_size_near",
        ] {
            println!("cargo:rustc-link-arg=-Wl,--wrap=snd_pcm_hw_params_{name}");
        }
    }
}
