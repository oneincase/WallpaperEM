// 可平铺双通道值噪声（近似 WE 内置 util/noise 的 RG 噪声，Phase 3 摆动/脉冲用）
export function generateNoiseTexture(size = 256, cellSize = 8) {
  const cells = size / cellSize
  const randA = mulberry32(1337)
  const randB = mulberry32(4242)
  const gridA = new Float32Array(cells * cells)
  const gridB = new Float32Array(cells * cells)
  for (let i = 0; i < cells * cells; i++) {
    gridA[i] = randA()
    gridB[i] = randB()
  }
  const rgba = new Uint8Array(size * size * 4)
  const cell = cellSize
  for (let y = 0; y < size; y++) {
    const cy = Math.floor(y / cell)
    const fy = (y % cell) / cell
    const sy = fy * fy * (3 - 2 * fy)
    for (let x = 0; x < size; x++) {
      const cx = Math.floor(x / cell)
      const fx = (x % cell) / cell
      const sx = fx * fx * (3 - 2 * fx)
      const x0 = cx % cells
      const x1 = (cx + 1) % cells
      const y0 = cy % cells
      const y1 = (cy + 1) % cells
      const a00 = gridA[y0 * cells + x0]
      const a10 = gridA[y0 * cells + x1]
      const a01 = gridA[y1 * cells + x0]
      const a11 = gridA[y1 * cells + x1]
      const b00 = gridB[y0 * cells + x0]
      const b10 = gridB[y0 * cells + x1]
      const b01 = gridB[y1 * cells + x0]
      const b11 = gridB[y1 * cells + x1]
      const va = (a00 + (a10 - a00) * sx + (a01 - a00) * sy + (a00 - a10 - a01 + a11) * sx * sy)
      const vb = (b00 + (b10 - b00) * sx + (b01 - b00) * sy + (b00 - b10 - b01 + b11) * sx * sy)
      const o = (y * size + x) * 4
      rgba[o] = Math.round(va * 255)
      rgba[o + 1] = Math.round(vb * 255)
      rgba[o + 2] = 0
      rgba[o + 3] = 255
    }
  }
  return rgba
}

function mulberry32(seed) {
  let a = seed >>> 0
  return () => {
    a |= 0
    a = (a + 0x6d2b79f5) | 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}
