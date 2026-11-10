fn main() {
 let prefix = std::env::var("DEP_PILOTDECK_IMAGE_NATIVE_PREFIX").expect("image native prefix");
 println!("cargo:rustc-link-search=native={prefix}/lib");
 println!("cargo:rustc-link-lib=vips");
 println!("cargo:rustc-link-lib=glib-2.0");
 println!("cargo:rustc-link-lib=gobject-2.0");
}
