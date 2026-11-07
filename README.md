# icyfix

Repair raw Shoutcast / Icecast (ICY) stream recordings so they play cleanly in any MP3 player, with track titles and timestamps preserved as **ID3v2 chapters** and a companion **`.cue` sheet**.

(Not sure the ID3v2 part works, couldn't find a player where it looked fine. The .cue index file works great in Foobar2000)

> **Vibe-coded with [Claude](https://claude.ai).**
> This project was written through a conversation with Claude (Anthropic's AI assistant). Review the code before relying on it, and please report any issues you find.

---

## The problem

When you record an internet radio stream directly (for example with `curl` or a simple socket dump), the file is not a valid MP3:

- It may start with the server's **HTTP/ICY response headers** (`ICY 200 OK`, `icy-metaint: 16000`, `icy-name: …`).
- Every `icy-metaint` bytes, the server inserts a **metadata block** (`StreamTitle='Artist - Title';`) **in the middle of the audio data**. These blocks corrupt MP3 frames, causing clicks, glitches, wrong durations or outright playback errors.
- The recording usually **starts and ends mid-frame**.
- Track titles are buried in the stream and invisible to players.

## What icyfix does

1. **Strips the HTTP/ICY header** and reads `icy-metaint`, `icy-name`, `icy-genre` and `icy-url` from it.
2. **Removes the interleaved metadata blocks** and extracts every `StreamTitle` change along with its position. If the header is missing, `icy-metaint` is **auto-detected**.
3. **Resynchronises on valid MPEG audio frames** (MPEG-1/2/2.5, Layer I/II/III), dropping truncated or garbage frames at the start, at the end and in between.
4. **Writes an ID3v2.3 tag** with:
   - `TPE1` / `TIT2`: artist and title (split on `" - "`)
   - `TRSN`: station name, `TCON`: genre, `WORS`: station URL
   - `CTOC` + `CHAP`: **one chapter per track**, with millisecond start/end times
5. **Writes a `.cue` sheet** next to the MP3 (same base name) listing every track with its `INDEX` timestamp.

Timestamps are computed from the frames actually kept, counted in samples, so they stay accurate for VBR streams and after any frames are dropped. ID3 chapters and the CUE sheet are always aligned.

## Building

Requires a Rust toolchain ([rustup.rs](https://rustup.rs)). On Windows, the MSVC build tools are also needed. No external crates are used.

    cargo build --release

The binary is written to `target/release/icyfix` (`icyfix.exe` on Windows).

## Usage

    icyfix <input.mp3> [output.mp3] [--metaint N]

| Argument | Description |
|---|---|
| `input.mp3` | Raw stream recording |
| `output.mp3` | Optional. Defaults to `<input>_fixed.mp3` in the same folder |
| `--metaint N` | Force the metadata interval, in bytes (overrides header and auto-detection) |

The original file is never modified.

### Example

    $ icyfix recording.mp3
    En-tête ICY/HTTP retiré : 312 octets
    Station : LOL.FM - Trance
    icy-metaint : 16000 (en-tête)
    Métadonnées retirées : 4821 octets
    Format : MPEG-1 Layer III, 44100 Hz, 128 kbit/s (1re trame)
    Trames : 47312, durée ≈ 00:20:35
    Octets écartés (trames coupées ou parasites) : 1187
    Titres :
      [00:00:00 → 00:03:47] Artist A - Title A
      [00:03:47 → 00:08:21] Artist B - Title B
      [00:08:21 → 00:20:35] Artist C - Title C
    Écrit : recording_fixed.mp3 (19765432 octets)
      3 chapitres ID3 (CHAP/CTOC)
    Écrit : recording_fixed.cue (3 pistes)

> Console output is currently in French.

### Generated `.cue`

    REM GENRE "Trance"
    PERFORMER "lol.FM - Trance Channel"
    TITLE "LOL.FM - Trance"
    FILE "recording_fixed.mp3" MP3
      TRACK 01 AUDIO
        TITLE "Title A"
        PERFORMER "Artist A"
        INDEX 01 00:00:00
      TRACK 02 AUDIO
        TITLE "Title B"
        PERFORMER "Artist B"
        INDEX 01 03:47:12
      ...

The CUE file is UTF-8 with BOM and CRLF line endings. `FILE` uses a relative name, so keep the `.mp3` and `.cue` together.

## Limitations

- **MPEG audio only.** AAC / AAC+ (ADTS) streams are not supported yet; icyfix reports an error if no valid MPEG frame is found.
- **Title timing is approximate.** ICY metadata is only sent at `icy-metaint` boundaries, so a title change can be reported up to one interval late (about 0.5–1 s at 128 kbit/s). This is a limit of the protocol.
- **CUE: 99 tracks max** (format limit). **ID3 chapters: 255 max** (`CTOC` limit).
- Metadata is decoded as UTF-8 if valid, otherwise as Latin-1. Other legacy encodings may display incorrectly.
- The top-level ID3 title uses the **first** track found.
- ID3 chapter support varies between players. Podcast apps generally handle it well; for music players, the `.cue` sheet is usually the better option.

## Possible improvements

- AAC / ADTS support
- Splitting the recording into one file per track
- English console output and `--quiet` / `--json` options
- Reading directly from a live stream URL

## License

GPLv3.0

## Change log

07 nov. 2026 : v1.0.0 : First commit. Program tested on W11 with 100% success rate on 92 files (~1gbps 50 songs/titles per file)


## More

__WARNING : Personal/specific commands. Use if you understand what you do.__

### I got my stream with this kind of command:
  
    #!/bin/sh
    
    while true 
    do
      echo ""
      echo "Restart at $(date) . "     #                                          I get 1gig files at each loop.
      curl -H "Icy-MetaData: 1" -i http://radio.lol.fm/trance_360kbps | dd bs=10000 count=100000 > LOL.FM-Trance-360kbps_$(date +%s).icecast_with_headers.mp3
    
      sleep 15
    done

### Renaming and removing the ".icecast_with_headers" string where it is : 

    for i in *with_head*; do   new=$(echo $i | sed "s/.icecast_with_headers_fixed//" ) ;   mv -v $i $new; done

    for i in *cue ; do   sed -i 's/\.icecast_with_headers_fixed//g'  $i ; done

### Change the modification time to the recording time 
    for i in $( ls *mp3 | tr . _  | cut -d _ -f 2 | sort -u )
    do
      touch -d @$i *$i*
    done
