// scene.pkg 容器解析器
// 格式（实测 PKGV0012 ~ PKGV0023）：
//   [0]    uint32 LE  魔数字符串长度（实测恒为 8）
//   [4]    n 字节     魔数 "PKGVxxxx"（版本号）
//   [4+n]  uint32 LE  入口数量
//   随后 count 个入口：{ uint32 nameLen, name 字节, uint32 offset, uint32 size }
//   dataStart = 入口表结束位置；入口数据 = dataStart + offset，长度 size。
//   offset 为相对 dataStart 的偏移（首个入口 offset 恒为 0）。
export function parsePkg(buf) {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  if (buf.length < 16) throw new Error('文件太小，不是 scene.pkg')
  const magicLen = dv.getUint32(0, true)
  if (magicLen < 4 || magicLen > 16) throw new Error('魔数长度异常: ' + magicLen)
  const magic = ascii(buf, 4, magicLen)
  if (!magic.startsWith('PKGV')) throw new Error('不是 scene.pkg，魔数: ' + magic)
  const count = dv.getUint32(4 + magicLen, true)
  let p = 4 + magicLen + 4
  const entries = []
  for (let i = 0; i < count; i++) {
    if (p + 8 > buf.length) throw new Error('入口表越界 @' + i)
    const nameLen = dv.getUint32(p, true)
    p += 4
    if (p + nameLen + 8 > buf.length) throw new Error('入口 ' + i + ' 越界')
    const name = decodeName(buf, p, nameLen)
    p += nameLen
    const offset = dv.getUint32(p, true)
    p += 4
    const size = dv.getUint32(p, true)
    p += 4
    entries.push({ name, offset, size })
  }
  const dataStart = p
  return {
    magic,
    version: magic.slice(4),
    count,
    entries,
    dataStart,
    fileSize: buf.length,
    buf,
  }
}

// 取指定路径的入口数据（副本）
export function getEntry(pkg, name) {
  const e = pkg.entries.find((x) => x.name === name)
  if (e === undefined) return null
  const start = pkg.dataStart + e.offset
  const end = start + e.size
  if (start < 0 || end > pkg.fileSize) throw new Error('入口 ' + name + ' 越界')
  return pkg.buf.subarray(start, end).slice()
}

// 入口数据末尾应恰好贴住文件末尾（结构自检）
export function verifyLayout(pkg) {
  let end = 0
  for (const e of pkg.entries) end = Math.max(end, e.offset + e.size)
  return { dataEnd: pkg.dataStart + end, fileSize: pkg.fileSize, ok: pkg.dataStart + end <= pkg.fileSize }
}

function ascii(buf, start, len) {
  let s = ''
  for (let i = start; i < start + len; i++) s += String.fromCharCode(buf[i])
  return s
}

// 入口名：WE 以 UTF-8 存储（中文名场景实测）；非法 UTF-8 时回退 Latin-1 逐字节
function decodeName(buf, start, len) {
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(buf.subarray(start, start + len))
  } catch (e) {
    return ascii(buf, start, len)
  }
}
