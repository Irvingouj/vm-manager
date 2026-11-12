fn main() {
    // The target triple names the theseus-rs archive for this build.
    println!("cargo:rustc-env=LP_TARGET={}", std::env::var("TARGET").unwrap());
}
