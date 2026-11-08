# Cadence Windows portability patch

Upstream: gpui-pre-windows 0.3.7, from crates.io; license: LICENSE-APACHE.

The only source change is in src/directx_renderer.rs: debug shaders are embedded
with include_str!, the alpha-correction include is inlined, and D3DCompile reads
memory instead of D3DCompileFromFile reading the build machine's Cargo registry.
Optimized builds continue using upstream's precompiled shaders without changes.

Keep this patch when updating GPUI until the upstream debug renderer is portable.
