# How the WO Mic client was reconstructed

This document records where every protocol constant in this repository comes
from. The reference implementation is the shipped Windows client of WO Mic,
specifically `WOMicClient.exe` with its engine library:

* `WOMicEngine.dll`, PE32 x86, 230 912 bytes
* SHA-256 `f30878843682841647a49513a03427fd95cb01bc53a235d592687590f4d2e746`
* debug path references `WOMicClient.pdb` (RSDS, GUID `68598c98-aab2-4d07-84f7-def85e9d97a9`)

The analysis was done statically with Ghidra 12.1.4. Function names below are
Ghidra names (`FUN_0040xxxx`, image base `0x00400000`); the pseudocode is a
decompiler view, not the original source. Everything in the *observed* lists
was read directly out of the disassembly or the import tables; the *design
choices* of this rewrite are listed separately at the end.

## Control channel

The control channel is a byte stream (TCP for the Wi-Fi and USB transports,
RFCOMM for Bluetooth). Every message is `[tag:1][length:u32 BE][payload]`,
both directions. Three commands exist and the client sends them in this
order:

| tag | command | payload | evidence |
|-----|---------|---------|----------|
| `0x65` (`e`) | connect | `04 04 00 00 00 00` | `FUN_00404eb0` writes `0x65`, the length `0x06000000` (big endian 6) and the six payload bytes one by one, then reads the reply and compares the first byte against `0x65` |
| `0x66` (`f`) | version check | `02 02` + transport id as `u32` big endian | `FUN_00405000`; the transport id from controller offset `+0x08` is byte-swapped into big endian before it is written |
| `0x67` (`g`) | start streaming | empty | `FUN_00405120` writes `0x67` and a zero length |

The connect reply carries two payload bytes: the first is a status (zero on
success) and the second is the media framing id. `FUN_00403ea0` passes
controller offset `+0x44` as the reply target, and the framing reader
dispatch later switches on the value stored there.

The master start routine `FUN_00403ea0` fixes the error codes this client
reports:

```c
iVar2 = FUN_004048a0(...);        // transport factory
if (uVar1 == 0) {                 // unknown transport -> -8
    iVar2 = FUN_00404eb0(...);    // connect
    if (iVar2 != 0) return (iVar2 != 10) - 7;   // 10 -> -7, else -6
    iVar2 = FUN_00405000(...);    // version check
    if (iVar2 != 0) return 0xfffffff9;          // -7
    *(int *)(this + 0x50) = 0x14;               // 20 ms frame duration
    *(int *)(this + 0x48) = 48000;              // sample rate
    *(int *)(this + 0x4c) = 1;                  // one channel
    iVar2 = FUN_00405120(...);    // start
    uVar1 = -(uint)(iVar2 != 0) & 0xfffffff8;   // -8 on failure
}
```

The media open failure code `-12` comes from the transport factory
(`FUN_004048a0`): after the control stream opens, the media stream's open
method is called and a `-1` return is mapped to `-0xc`.

## Transports

The transport factory `FUN_004048a0` switches on the transport id from
`settings.dat` and builds two stream objects per connection:

| id | control stream | media stream | vtables |
|----|----------------|--------------|---------|
| 1 Bluetooth | `BtDeviceStream` | `BluetoothMediaStream` | `0x00422e84` / `0x00422f1c` |
| 2 USB | `UsbControlStream` | `UsbMediaStream` | `0x00422eb4` / `0x00422f38` |
| 3 Wi-Fi | `WinSockStream` | `WifiMediaStream` | `0x00422e9c` / `0x00422f00` |
| 4 Wi-Fi Direct | `WinSockStream` | `WifiMediaStream` | same as Wi-Fi |

`BluetoothMediaStream` and `UsbMediaStream` share one vtable
(`0x00403490...`): both are thin wrappers around their control stream. The
media frame reader `FUN_00404730` reads every frame through the controller's
stream at offset `+0x18`, so Bluetooth and USB carry control and media over
one connection. Only the Wi-Fi transports open a second channel (the
`WifiMediaStream` object is 0x410 bytes and holds its own socket from the
configured media port).

## Media framing

The framing id from the connect reply selects one of three frame readers.
All integers are big endian; the largest payload buffer on the controller is
`0x790 - 0x10 = 0x780` bytes.

| id | layout | reader |
|----|--------|--------|
| 2 (USB) | `[hdr0][hdr1][seq:u16][len:u16][payload]` | `FUN_00404db0` |
| 3 (Bluetooth) | `[hdr0][hdr1][seq:u16][len:u16][payload]` | `FUN_00404c90` |
| 4 (Wi-Fi) | `[04][00][len:u16][seq:u16][ts:u32][flag:u8][payload]`, `len` counts everything after itself | `FUN_00404af0` |

`FUN_00404af0` checks the `04 00` prefix, reads `len`, then seq, timestamp
and flag, and hands the remaining `len - 7` bytes to the decoder. For the
Wi-Fi transport the frames arrive as UDP datagrams, so one datagram is one
frame. `FUN_00404580` (the Wi-Fi media thread) tracks a play time and drops
packets that arrive after their slot was played.

## Codec and sample format

The media payload is Opus. The controller object carries
`OpusVoiceDecoder::vftable` (the class name survives in the RTTI data); the
decoder object is created in the media thread `FUN_004042d0` with the sample
rate and channel count that `FUN_00403ea0` stored at offsets `+0x48` and
`+0x4c`: 48000 Hz, one channel. A frame is 20 ms (`+0x50 = 0x14`), so one
decoded frame is 960 samples = 1920 bytes of signed 16 bit PCM.

The Opus decoder is statically linked into the DLL (there is no `opus`
import). The library's rate validation accepts 48000, 24000, 16000 and 12000
Hz (`FUN_0040ad00`), and the decoder setup paths allocate the 960 sample
frame (`FUN_0040edb0(48000, 0x3c0, ...)`).

Playback goes through `waveOut` (`WINMM.dll` imports: `waveOutOpen`,
`waveOutPrepareHeader`, `waveOutWrite`, ...). The player initialises ten
buffers of 0x780 bytes (`FUN_00401f10`: 48000 at `+0x154`, 0x14 at `+0x158`,
a ten iteration `operator_new(0x780)` loop) and opens the device with:

```c
WAVEFORMATEX {
    wFormatTag     = 1,        // PCM
    nChannels      = 1,
    nSamplesPerSec = 48000,    // from player offset +0x154
    wBitsPerSample = 16,
    nBlockAlign    = 2,
    nAvgBytesPerSec = nSamplesPerSec * 2,
}
waveOutOpen(handle, WAVE_MAPPER, &format, callback, instance, CALLBACK_FUNCTION);
```

(`FUN_00401f70`.)

## Bluetooth connection

The Bluetooth path in `WOMicEngine.dll` is a straight port of Microsoft's
Bluetooth service discovery sample:

1. Paired devices are enumerated with `BluetoothFindFirstDevice`
   (`FUN_00402210`), `fReturnRemembered = 1`; each device is stored as
   address plus name.
2. For the chosen device, a `WSALookupServiceBeginW` query
   (`FUN_00402760`) looks up the Serial Port Profile service class
   `{00001101-0000-1000-8000-005F9B34FB}` on that device's `SOCKADDR_BTH`,
   with `LUP_FLUSHCACHE | LUP_RETURN_NAME | LUP_RETURN_TYPE |
   LUP_RETURN_ADDR | LUP_RETURN_BLOB | LUP_RETURN_COMMENT`.
3. For each result, `FUN_00402570` reads SDP attribute 4 (Protocol
   Descriptor List), walks the protocol descriptors until it finds the one
   whose UUID is RFCOMM (3), and `FUN_004024f0` reads the channel byte from
   its second element.
4. `FUN_00402350` connects: `socket(AF_BTH /* 0x20 */, SOCK_STREAM,
   BTPROTO_RFCOMM /* 3*/)` and `connect` with a 30 byte `SOCKADDR_BTH`
   (family, 8 byte address, zero service class GUID, port = channel).

## Settings file

`settings.dat` next to the executable is plain text, one `key:value` record
per line. The shipped file of client 4.x:

```text
transportType:3
wifiServerAddr:192.168.1.178
softApAddr:192.168.43.1
playLocally:1
controlPort:1488
mediaPort:49152
bufferSize:3
autoConnect:0
window:712,403,500,357,1972
btAddress:0.0.0.0.0.0
```

This client reads and writes the same keys, so a configuration works with
both implementations.

## Design choices in this rewrite

These are deliberate deviations, not observations:

| area | original client | this rewrite |
|------|-----------------|--------------|
| Bluetooth channel discovery | SDP lookup of the SPP service, channel from the protocol descriptor list | scans channels 1..=30, or dials an explicit channel with `--bt-channel`; the WO Mic service is the only SPP listener on the phone, so the first successful connect is its channel |
| Audio output | `waveOut`, optionally through its own virtual audio device driver | cpal (WASAPI on Windows, ALSA on Linux), or raw PCM to stdout for piping into another program |
| Jitter buffer | play time per packet, late packets dropped (`FUN_00404580`) | reorders by sequence number, releases the oldest packet when the buffer is full, skips gaps once play out has started, exposes the same counters |
| Reconnect | retries a broken stream (code -20) from its main loop | reconnects after a configurable pause, driven by the same "aborted" error class |

## Not verified against a live server

* The exact behaviour of the USB transport end to end. The analysis shows
  one forwarded port and one shared connection, but only a real phone
  confirms it.
* Whether the server ever uses framings other than the announced one, and
  what the second byte of the Wi-Fi frame flag means.
* Server side reconnect semantics after `g` is sent twice.
