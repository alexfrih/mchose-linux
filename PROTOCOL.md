# MCHOSE HID configuration protocol

Recovered from the M HUB web driver bundle (`mchose.com.cn`, chunk
`purify.es-Dc-oxOSy.js`, deobfuscated in `protocol/`). Covers the MCHOSE mice
that enumerate under vendor `0x5253` (RealTek-assigned) and the newer `0x3837`,
which the driver treats identically: L7 / L7 Pro / L7 Ultra, M7, A7, K7.

Verified device here: **MCHOSE L7 Pro**, USB `5253:1020` on the dongle,
wireless device ID `00b0` (176, which is what the driver's own model table
lists for this mouse), firmware 5.12.2.1, 26000 DPI sensor, LOD 1 or 2 mm.
Everything below was exercised against that mouse on 2026-08-28: the frame,
the inversion, and every offset in the config block. The six DPI stages came
back as 400 / 800 / 1600 / 3200 / 6400 / 26000, the factory preset, which is
what pins the layout.

## Interface

Three USB interfaces. Configuration lives on the third one, the vendor
collection: usage pages `0xFF01` (65281) and `0xFF0B` (65291). On this machine
that is `/dev/hidraw14`; never hardcode the number, match the report descriptor.

Two report IDs carry the whole protocol:

| Report | Payload | Use |
|---|---|---|
| `0x11` | 20 bytes | short commands (rate, LOD, sleep, DPI, profile, RGB) |
| `0x12` | 64 bytes | long commands (full config, per-axis DPI, macros) |

Both are **feature reports**: `HIDIOCSFEATURE` to write, `HIDIOCGFEATURE` to
read back. Output reports are not used by this protocol.

## Frame

Request payload, before inversion:

```
byte 0    command
byte 1..  arguments, per schema below
rest      zero
```

Then **every payload byte is inverted (`^ 0xFF`)**, padding included, so an
all-zero tail goes on the wire as `FF FF FF ...`.

Response, from `HIDIOCGFEATURE` on the same report ID:

```
byte 0    report ID          (not inverted)
byte 1    command ^ 0xFF     (echo, use it to match the reply)
byte 2..  payload ^ 0xFF
```

There is no checksum. The driver retries a read up to 6 times when the echoed
command does not match, and up to 5 times on a busy device.

## Read commands

| Report | Cmd | Fields |
|---|---|---|
| `0x11` | `0x03` | `bond:u8`, `vid:u16`, `pid:u16`, `connect:u8`, `game:u8` |
| `0x11` | `0x04` | `versionLength:u8`, `version:char[versionLength]` |
| `0x11` | `0x06` | `vid:u16`, `pid:u16`, `fwVersion:u32`, `connectMode:bit3`, `connectStatus:bit1`, `_:bit4`, `batteryLevel:u8`, `chargeStatus:u8` |
| `0x11` | `0x1b` | RGB: `enable`, `id`, `useIndependent`, `brightness`, `lightSetting`, `closeTime`, `speed`, `openRandomColor`, `red`, `green`, `blue`, `red2`, `green2`, `blue2`, `changeDirect` |
| `0x12` | `0x67` | full config, see below |
| `0x12` | `0x65` / `0x77` / `0x63` | macro body / macro data / macro name |

All multi-byte integers are little-endian.

### `0x12 0x67`, the full config

```
profileIndex   u8
gDpiIndex      bit4     active DPI stage on the 2.4 GHz link
gRateIndex     bit4     report rate index on the 2.4 GHz link
usbDpiIndex    bit4     active DPI stage when wired
usbRateIndex   bit4     report rate index when wired
reserved       u8
dpi0..dpi5     u16 x6   the six DPI stages, raw DPI (e.g. 1600), not encoded
dpiSum         u8       how many stages are enabled
sensor         u8
keyDebounce    u8       ms
sleep          u8       minutes
button1..6     bit4 index + bit4 type + u24 value, per button
reserved1..5   u8 x5
rotateVal      u8
val            u8
dpiVal0..5     u16 x6   Y-axis DPI when the stages are not square
```

Nibble order on the wire is `usbRateIndex, usbDpiIndex, gRateIndex, gDpiIndex`
(the write schema is authoritative; the read parser lists them the other way
round because it reads the low nibble first).

## Write commands

| Report | Cmd | Schema |
|---|---|---|
| `0x11` | `0x02` | `game:u8`, the game mode. Reads back at `0x11 0x03` byte 6. |
| `0x11` | `0x0A` | `sleepStatus:u8`, `sleep:u8` |
| `0x11` | `0x0B` | `data:u16` |
| `0x11` | `0x2B` | RGB, same 15 fields as read `0x1b` |
| `0x11` | `0x40` | `usbDpiIndex:u8`, `gDpiIndex:u8`, `reserved:u8`, `dpi0..5:u16`, `sum:u8` |
| `0x11` | `0x41` | `usbRate:u8`, `freeRate:u8` |
| `0x11` | `0x42` | `lod:u8`, `ripple:u8`, `line:u8`, `motionSync:u8`, `_:u8`, `_:u8`, `gameMode:u8`, `rotateOpen:u8`, `rotateVal:u8` |
| `0x11` | `0x43` | `time:u8` |
| `0x11` | `0x58` | `profileIndex:u8` |
| `0x12` | `0x40` | `0x11 0x40` plus `diff:u8` and `dpiVal0..5:u16` (per-axis DPI) |
| `0x12` | `0x52` | `buttonIndex:u8`, `_:u8`, `buttonType:u8`, `buttonValue:u24` |
| `0x12` | `0x55` | macro chunk: `buttonIndex`, `moreData`, `offset:u16`, `length` |
| `0x12` | `0x57` | the whole config, same layout as read `0x12 0x67` |
| `0x12` | `0x2D` | RGB gradient |

DPI is written as the **raw value** in a `u16`, little-endian. There is no
`dpi/50 - 1` encoding here; that belongs to the other MCHOSE protocol family
(templated frames with a sum checksum, used by the devices with a screen).

## Report rate index

`usbRate` / `freeRate` index into the model's own list. For an 8K model:

```
0 -> 125 Hz   1 -> 500 Hz   2 -> 1000 Hz
3 -> 2000 Hz  4 -> 4000 Hz  5 -> 8000 Hz
```

A 1K-only link stops at index 2. The L7 Pro is 8K on the dongle and wired, 1K
on Bluetooth.

## LOD

`lod` in `0x11 0x42` is an index into the model's list. L7 Pro: `0 -> 1 mm`,
`1 -> 2 mm`. The 42000 DPI models start at 0.7 mm instead.

## Model table

From the same bundle, per model: the DPI ceiling, the LOD list, and the rate
list per link. L7 Pro: `pids ["176", "4129", "4128"]`, `dpiMax 26000`,
`lod [1mm, 2mm]`, rate `[8K, 1K, 8K]`.

## What the hardware taught us

Six things the bundle does not say, found by driving the real mouse.

**Game mode has its own command.** `0x11 0x02 <mode>` is what moves the `game`
byte in `0x11 0x03`. The `gameMode` field inside `0x11 0x42` is something else
and leaves that byte alone, so do not use it.

**`0x11 0x42` is not write-only after all.** Its ripple, angle-snap and motion
sync arguments land in the config block's `sensor` byte (body offset 17) as
bits 2, 3 and 4. Bit 0 belongs to the mouse and is always set. So the flags can
be read back, even though there is no read command for them.

**They do not clear the same way they set.** Sending `0x11 0x42` with zeroes
leaves the bits standing, and the mouse rejects a config-block write that
clears `sensor` outright: it answers with the byte it had. Turning one of them
off needs more than the obvious inverse, which is why this tool still keeps the
last written set in its own state file.

**Wired is body[1], wireless is body[2].** The vendor's read parser and write
schema disagree; setting the rate over the dongle moved only body[2], which
settles it.

**Two bytes of the config block belong to the mouse.** Offset 3, the one the
vendor schema calls `reserved`, flips between `0x00` and `0x02` on its own
whenever the active DPI stage changes, and bit 0 of `sensor` (offset 17) is
always set. Comparing a read-back byte for byte against what was written
therefore reports a failure on writes that landed perfectly.

**A config write needs time to commit.** Reading the block straight back after
`0x12 0x57` returns the old contents often enough to look like a failed write.
Poll for a few hundred milliseconds instead.

**The mouse can be talked at too fast.** Applying a preset first took fifteen
exchanges inside a second and choked the config channel every time. A 25 ms
floor between transactions, process-wide, makes it reliable; without it no
amount of retrying helps.

**The long-report channel can wedge.** `0x12 0x67` starts answering 63 zero
bytes while `0x11 0x03` and `0x11 0x06` keep working normally, so it reads like
a wiped mouse when nothing was lost. Re-selecting the profile with
`0x11 0x58` brings it straight back.
