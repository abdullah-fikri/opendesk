// Area-average downscale of the captured desktop (BGRA, read as RGBA) into an RGBA texture, the GPU twin of the
// CPU scaler in `capture::win::dxgi` (`update_region`): every output pixel is the mean of its exact source
// footprint, partial edge pixels weighted by coverage. Compiled to `area_scale.cso` by
// `cargo test -p core-engine --lib compile_area_scale_shader -- --ignored` (cs_5_0).
Texture2D<float4> Src : register(t0);
RWTexture2D<unorm float4> Dst : register(u0);

cbuffer Params : register(b0)
{
    float2 Scale; // source pixels per output pixel
    uint2 DstSize;
    uint2 SrcSize;
    float2 Pad;
};

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID)
{
    if (id.x >= DstSize.x || id.y >= DstSize.y)
        return;
    float x0 = id.x * Scale.x, x1 = min(x0 + Scale.x, SrcSize.x);
    float y0 = id.y * Scale.y, y1 = min(y0 + Scale.y, SrcSize.y);
    uint xe = (uint)ceil(x1), ye = (uint)ceil(y1);
    float4 acc = 0;
    float area = 0;
    for (uint y = (uint)y0; y < ye; y++)
    {
        float wy = min(y + 1.0, y1) - max((float)y, y0);
        for (uint x = (uint)x0; x < xe; x++)
        {
            float w = (min(x + 1.0, x1) - max((float)x, x0)) * wy;
            acc += Src.Load(int3(x, y, 0)) * w;
            area += w;
        }
    }
    Dst[id.xy] = float4(acc.rgb / area, 1.0);
}
