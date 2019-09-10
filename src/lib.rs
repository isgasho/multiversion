//! This crate provides the [`target_clones`] attribute and [`multiversion!`] macro for
//! implementing function multiversioning.
//!
//! Many CPU architectures have a variety of instruction set extensions that provide additional
//! functionality. Common examples are single instruction, multiple data (SIMD) extensions such as
//! SSE and AVX on x86/x86-64 and NEON on ARM/AArch64. When available, these extended features can
//! provide significant speed improvements to some functions. These optional features cannot be
//! haphazardly compiled into programs–executing an unsupported instruction will result in a
//! crash.  Function multiversioning is the practice of compiling multiple versions of a function
//! with various features enabled and safely detecting which version to use at runtime.
//!
//! # Target specification strings
//! Targets for both the [`target_clones`] attribute and the [`multiversion!`] macro are specified
//! as a combination of architecture (as specified in the `target_arch` attribute) and feature (as
//! specified in the `target_feature` attribute). A single architecture can be specified as:
//! * `"arch"`
//! * `"arch+feature"`
//! * `"arch+feature1+feature2"`
//!
//! while multiple architectures can be specified as:
//! * `"[arch1|arch2]"`
//! * `"[arch1|arch2]+feature"`
//! * `"[arch1|arch2]+feature1+feature2"`
//!
//! The following are all valid target specification strings:
//! * `"x86"` (matches the `"x86"` architecture)
//! * `"x86_64+avx+avx2"` (matches the `"x86_64"` architecture with the `"avx"` and `"avx2"`
//! features)
//! * `"[mips|mips64|powerpc|powerpc64]"` (matches any of the `"mips"`, `"mips64"`, `"powerpc"` or
//! `"powerpc64"` architectures)
//! * `"[arm|aarch64]+neon"` (matches either the `"arm"` or `"aarch64"` architectures with the
//! `"neon"` feature)
//!
//! # Example
//! The following example is a good candidate for optimization with SIMD.  The function `square`
//! optionally uses the AVX instruction set extension on x86 or x86-64.  The SSE instruciton set
//! extension is part of x86-64, but is optional on x86 so the square function optionally detects
//! that as well.  This is automatically implemented by the [`target_clones`] attribute.
//!
//! This is works by compiling multiple *clones* of the function with various features enabled and
//! detecting which to use at runtime. If none of the targets match the current CPU (e.g. an older
//! x86-64 CPU, or another architecture such as ARM), a clone without any features enabled is used.
//! ```
//! use multiversion::target_clones;
//!
//! #[target_clones("[x86|x86_64]+avx", "x86+sse")]
//! fn square(x: &mut [f32]) {
//!     for v in x {
//!         *v *= *v;
//!     }
//! }
//! ```
//!
//! The following produces a nearly identical function, but instead of cloning the function, the
//! implementations are manually specified. This is typically more useful when the implementations
//! aren't identical, such as when using explicit SIMD instructions instead of relying on compiler
//! optimizations. The multiversioned function is generated by the [`multiversion!`] macro.
//! ```
//! use multiversion::multiversion;
//!
//! multiversion!{
//!     fn square(x: &mut [f32])
//!     "[x86|x86_64]+avx" => square_avx,
//!     "x86+sse" => square_sse,
//!     default => square_generic,
//! }
//!
//! #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
//! #[target_feature(enable = "avx")]
//! unsafe fn square_avx(x: &mut [f32]) {
//!     for v in x {
//!         *v *= *v;
//!     }
//! }
//!
//! #[cfg(target_arch = "x86")]
//! #[target_feature(enable = "sse")]
//! unsafe fn square_avx(x: &mut [f32]) {
//!     for v in x {
//!         *v *= *v;
//!     }
//! }
//!
//! fn square_generic(x: &mut [f32]) {
//!     for v in x {
//!         *v *= *v;
//!     }
//! }
//!
//! # fn main() {}
//! ```
//!
//! # Implementation details
//! The function version dispatcher consists of a function selector and an atomic function pointer.
//! On the first invocation of a multiversioned function, the dispatcher loads the atomic and since
//! it's null, invokes the function selector. The result of the function selector is stored in the
//! atomic, then invoked. On subsequent calls, the atomic is not null and the contents are invoked.
//!
//! Some comments on the benefits of this implementation:
//! * The function selector is only invoked once. Subsequent calls are reduced to an atomic load,
//! branch, and indirect function call.
//! * If called in multiple threads, there is no contention. It is possible for two threads to hit
//! the same function before function selection has completed, which results in each thread
//! invoking the function selector, but the atomic ensures that these are synchronized correctly.
//!
//! [`target_clones`]: attr.target_clones.html
//! [`multiversion!`]: macro.multiversion.html

extern crate proc_macro;

mod dispatcher;
mod multiversion;
mod target;
mod target_clones;

use quote::ToTokens;
use syn::{parse_macro_input, ItemFn};

/// Provides function multiversioning by explicitly specifying function versions.
///
/// Functions are selected in order, calling the first matching target.  The final function must
/// have the `default` target, which indicates that this function does not require any special features.
///
/// # Safety
/// Functions compiled with the `target_feature` attribute must be marked unsafe, since calling
/// them on an unsupported CPU results in a crash.  The `multiversion!` macro will produce a safe
/// function that calls `unsafe` function versions, and the safety contract is fulfilled as long as
/// your specified targets are correct.  If your function versions are `unsafe` for any other
/// reason, you must remember to mark your generated function `unsafe` as well.
///
/// # Examples
/// ## A simple feature-specific function
/// This example creates a function `where_am_i` that prints the detected CPU feature.
/// ```
/// use multiversion::multiversion;
///
/// multiversion!{
///     fn where_am_i()
///     "[x86|x86_64]+avx" => where_am_i_avx,
///     "x86+sse" => where_am_i_sse,
///     "[arm|aarch64]+neon" => where_am_i_neon,
///     default => where_am_i_generic,
/// }
///
/// fn where_am_i_avx() {
///     println!("avx");
/// }
///
/// fn where_am_i_sse() {
///     println!("sse");
/// }
///
/// fn where_am_i_neon() {
///     println!("neon");
/// }
///
/// fn where_am_i_generic() {
///     println!("generic");
/// }
///
/// # fn main() {}
/// ```
/// ## Making `target_feature` functions safe
/// This example is the same as the above example, but calls `unsafe` specialized functions.  Note
/// that the `where_am_i` function is still safe, since we know we are only calling specialized
/// functions on supported CPUs.
/// ```
/// use multiversion::multiversion;
///
/// multiversion!{
///     fn where_am_i()
///     "[x86|x86_64]+avx" => where_am_i_avx,
///     "x86+sse" => where_am_i_sse,
///     "[arm|aarch64]+neon" => where_am_i_neon,
///     default => where_am_i_generic,
/// }
///
/// #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
/// #[target_feature(enable = "avx")]
/// unsafe fn where_am_i_avx() {
///     println!("avx");
/// }
///
/// #[cfg(target_arch = "x86")]
/// #[target_feature(enable = "sse")]
/// unsafe fn where_am_i_sse() {
///     println!("sse");
/// }
///
/// #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
/// #[target_feature(enable = "neon")]
/// unsafe fn where_am_i_neon() {
///     println!("neon");
/// }
///
/// fn where_am_i_generic() {
///     println!("generic");
/// }
///
/// # fn main() {}
/// ```
#[proc_macro]
pub fn multiversion(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    parse_macro_input!(input as multiversion::MultiVersion)
        .into_token_stream()
        .into()
}

/// Provides automatic function multiversioning by compiling *clones* of the function for each
/// target.
///
/// The proper function clone is invoked depending on runtime CPU feature detection.  Priority is
/// evaluated left-to-right, selecting the first matching target.  If no matching target is found,
/// a clone with no required features is called.
/// # Example
/// The function `square` runs with AVX or SSE compiler optimizations when detected on the CPU at
/// runtime.
/// ```
/// use multiversion::target_clones;
///
/// #[target_clones("[x86|x86_64]+avx", "x86+sse")]
/// fn square(x: &mut [f32]) {
///     for v in x {
///         *v *= *v;
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn target_clones(
    attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let config = parse_macro_input!(attr as target_clones::Config);
    let func = parse_macro_input!(input as ItemFn);
    target_clones::TargetClones::new(config, &func)
        .into_token_stream()
        .into()
}
