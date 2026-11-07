//! icyfix : répare un enregistrement brut de flux Shoutcast/Icecast (ICY).
//!
//! - retire l'en-tête HTTP/ICY éventuel en début de fichier ;
//! - retire les blocs de métadonnées insérés tous les `icy-metaint` octets ;
//! - resynchronise sur des trames MPEG audio valides (début/fin coupés, octets parasites) ;
//! - écrit un tag ID3v2.3 : artiste/titre, station, et un chapitre (CHAP/CTOC) par titre ;
//! - écrit un fichier .cue (même nom que le MP3 de sortie) listant les titres et leurs horodatages.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Default)]
struct IcyHeaders {
    metaint: Option<usize>,
    name: Option<String>,
    genre: Option<String>,
    url: Option<String>,
}

struct Track {
    audio_offset: usize,
    title: String,
}

/// Un titre et ses bornes, en nombre d'échantillons depuis le début de l'audio.
struct Chapter<'a> {
    start: u64,
    end: u64,
    title: &'a str,
}

#[derive(Clone, Copy)]
struct FrameHdr {
    len: usize,
    kbps: u32,
    sample_rate: u32,
    version: u8, // 3 = MPEG-1, 2 = MPEG-2, 0 = MPEG-2.5
    layer: u8,   // 3 = Layer I, 2 = Layer II, 1 = Layer III
}

impl FrameHdr {
    fn compatible(&self, o: &FrameHdr) -> bool {
        self.version == o.version && self.layer == o.layer && self.sample_rate == o.sample_rate
    }

    fn samples(&self) -> u32 {
        match (self.layer, self.version) {
            (3, _) => 384,
            (2, _) => 1152,
            (1, 3) => 1152,
            _ => 576,
        }
    }

    fn describe(&self) -> String {
        let v = match self.version { 3 => "MPEG-1", 2 => "MPEG-2", _ => "MPEG-2.5" };
        let l = match self.layer { 3 => "Layer I", 2 => "Layer II", _ => "Layer III" };
        format!("{v} {l}, {} Hz, {} kbit/s (1re trame)", self.sample_rate, self.kbps)
    }
}

// ---------------------------------------------------------------- utilitaires

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Les métadonnées ICY n'ont pas d'encodage défini : UTF-8 si valide, sinon Latin-1.
fn decode_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// « Artiste - Titre » → (Some(artiste), titre) ; sinon (None, texte entier).
fn split_title(t: &str) -> (Option<&str>, &str) {
    match t.split_once(" - ") {
        Some((a, s)) if !a.trim().is_empty() && !s.trim().is_empty() => (Some(a.trim()), s.trim()),
        _ => (None, t.trim()),
    }
}

// ---------------------------------------------------------------- en-tête ICY

fn parse_http_headers(data: &[u8]) -> (usize, IcyHeaders) {
    let mut h = IcyHeaders::default();
    if !(data.starts_with(b"ICY ") || data.starts_with(b"HTTP/")) {
        return (0, h);
    }
    let head = &data[..data.len().min(16 * 1024)];
    let end = match find(head, b"\r\n\r\n") {
        Some(p) => p + 4,
        None => match find(head, b"\n\n") {
            Some(p) => p + 2,
            None => return (0, h),
        },
    };
    for line in decode_text(&data[..end]).lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().to_string();
            match k.trim().to_ascii_lowercase().as_str() {
                "icy-metaint" => h.metaint = v.parse().ok(),
                "icy-name" => h.name = Some(v),
                "icy-genre" => h.genre = Some(v),
                "icy-url" => h.url = Some(v),
                _ => {}
            }
        }
    }
    (end, h)
}

// ---------------------------------------------------------------- métadonnées entrelacées

fn plausible_meta(blk: &[u8]) -> bool {
    let k = blk.len().min(12);
    let j = blk.len().min(10);
    blk[..k] == b"StreamTitle="[..k] || blk[..j] == b"StreamUrl="[..j]
}

/// Vérifie qu'un intervalle `m` donne une chaîne cohérente de blocs sur tout le fichier.
fn score_metaint(body: &[u8], m: usize) -> Option<usize> {
    let (mut pos, mut hits) = (0usize, 0usize);
    loop {
        pos += m;
        if pos >= body.len() {
            break;
        }
        let l = body[pos] as usize * 16;
        pos += 1;
        if l > 0 {
            let end = (pos + l).min(body.len());
            if !plausible_meta(&body[pos..end]) {
                return None;
            }
            hits += 1;
        }
        pos += l;
    }
    (hits > 0).then_some(hits)
}

fn detect_metaint(body: &[u8]) -> Option<usize> {
    let mut cands = vec![8192, 16000, 16384, 24576, 32768, 45000, 65536];
    if let Some(p) = find(body, b"StreamTitle='") {
        if p > 0 {
            cands.insert(0, p - 1); // cas fréquent : le 1er bloc contient déjà le titre
        }
    }
    cands.into_iter().chain(256..=65536).find(|&m| score_metaint(body, m).is_some())
}

fn meta_field(meta: &str, key: &str) -> Option<String> {
    let tag = format!("{key}='");
    let start = meta.find(&tag)? + tag.len();
    let rest = &meta[start..];
    let end = rest.find("';").or_else(|| rest.rfind('\''))?;
    Some(rest[..end].trim().to_string())
}

/// Sépare l'audio des blocs de métadonnées. Renvoie (audio, titres, octets retirés).
fn demux(body: &[u8], m: usize) -> (Vec<u8>, Vec<Track>, usize) {
    let mut audio = Vec::with_capacity(body.len());
    let mut tracks: Vec<Track> = Vec::new();
    let (mut pos, mut removed) = (0usize, 0usize);
    while pos < body.len() {
        let n = m.min(body.len() - pos);
        audio.extend_from_slice(&body[pos..pos + n]);
        pos += n;
        if pos >= body.len() {
            break;
        }
        let l = body[pos] as usize * 16;
        pos += 1;
        let end = (pos + l).min(body.len());
        let raw = &body[pos..end];
        removed += 1 + raw.len();
        pos = end;
        let raw = match raw.iter().rposition(|&b| b != 0) {
            Some(k) => &raw[..=k],
            None => continue,
        };
        if let Some(t) = meta_field(&decode_text(raw), "StreamTitle") {
            if !t.is_empty() && tracks.last().map_or(true, |last| last.title != t) {
                tracks.push(Track { audio_offset: audio.len(), title: t });
            }
        }
    }
    (audio, tracks, removed)
}

// ---------------------------------------------------------------- trames MPEG audio

fn parse_frame(b: &[u8]) -> Option<FrameHdr> {
    if b.len() < 4 || b[0] != 0xFF || (b[1] & 0xE0) != 0xE0 {
        return None;
    }
    let version = (b[1] >> 3) & 3;
    let layer = (b[1] >> 1) & 3;
    if version == 1 || layer == 0 {
        return None;
    }
    let bi = (b[2] >> 4) as usize;
    let si = ((b[2] >> 2) & 3) as usize;
    let pad = ((b[2] >> 1) & 1) as u32;
    if bi == 0 || bi == 15 || si == 3 || (b[3] & 3) == 2 {
        return None;
    }
    const SR: [[u32; 3]; 4] = [[11025, 12000, 8000], [0; 3], [22050, 24000, 16000], [44100, 48000, 32000]];
    const BR_V1: [[u32; 15]; 3] = [
        [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
        [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
    ];
    const BR_V2: [[u32; 15]; 3] = [
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ];
    let mpeg1 = version == 3;
    let li = (3 - layer) as usize; // 0 = Layer I, 1 = Layer II, 2 = Layer III
    let kbps = if mpeg1 { BR_V1[li][bi] } else { BR_V2[li][bi] };
    let sr = SR[version as usize][si];
    let len = match li {
        0 => (12 * kbps * 1000 / sr + pad) * 4,
        1 => 144 * kbps * 1000 / sr + pad,
        _ if mpeg1 => 144 * kbps * 1000 / sr + pad,
        _ => 72 * kbps * 1000 / sr + pad,
    } as usize;
    Some(FrameHdr { len, kbps, sample_rate: sr, version, layer })
}

/// Trame valide en `i` suivie d'au plus 3 trames compatibles (évite les fausses synchros).
fn confirmed(a: &[u8], i: usize, reference: Option<FrameHdr>) -> Option<FrameHdr> {
    let f = parse_frame(&a[i..])?;
    if let Some(r) = reference {
        if !r.compatible(&f) {
            return None;
        }
    }
    let mut j = i + f.len;
    for _ in 0..3 {
        if j + 4 > a.len() {
            break;
        }
        let g = parse_frame(&a[j..])?;
        if !f.compatible(&g) {
            return None;
        }
        j += g.len;
    }
    Some(f)
}

/// Ne garde que des trames complètes.
/// Renvoie (trames, octets écartés, 1re trame, nb trames, échantillons, index).
/// `index` associe l'offset audio de chaque trame conservée au nombre d'échantillons qui la précèdent.
fn clean_frames(a: &[u8]) -> (Vec<u8>, usize, Option<FrameHdr>, u64, u64, Vec<(usize, u64)>) {
    let mut out = Vec::with_capacity(a.len());
    let mut index = Vec::new();
    let (mut i, mut dropped, mut count, mut samples) = (0usize, 0usize, 0u64, 0u64);
    let mut reference: Option<FrameHdr> = None;
    while i < a.len() {
        let direct = reference.and_then(|r| parse_frame(&a[i..]).filter(|f| r.compatible(f)));
        let f = match direct {
            Some(f) => f,
            None => match (i..a.len()).find_map(|j| confirmed(a, j, reference).map(|f| (j, f))) {
                Some((j, f)) => {
                    dropped += j - i;
                    i = j;
                    f
                }
                None => {
                    dropped += a.len() - i;
                    break;
                }
            },
        };
        if i + f.len > a.len() {
            dropped += a.len() - i; // dernière trame tronquée
            break;
        }
        index.push((i, samples));
        out.extend_from_slice(&a[i..i + f.len]);
        i += f.len;
        count += 1;
        samples += f.samples() as u64;
        reference.get_or_insert(f);
    }
    (out, dropped, reference, count, samples, index)
}

// ---------------------------------------------------------------- ID3v2.3

fn id3_frame(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(10 + payload.len());
    f.extend_from_slice(id);
    f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(payload);
    f
}

fn id3_text(id: &[u8; 4], text: &str) -> Vec<u8> {
    let mut p = Vec::new();
    if text.is_ascii() {
        p.push(0); // ISO-8859-1
        p.extend_from_slice(text.as_bytes());
    } else {
        p.push(1); // UTF-16 avec BOM
        p.extend_from_slice(&[0xFF, 0xFE]);
        for u in text.encode_utf16() {
            p.extend_from_slice(&u.to_le_bytes());
        }
    }
    id3_frame(id, &p)
}

/// TPE1 + TIT2 si le titre est de la forme « Artiste - Titre », sinon TIT2 seul.
fn title_frames(t: &str) -> Vec<u8> {
    let mut v = Vec::new();
    let (artist, song) = split_title(t);
    if let Some(a) = artist {
        v.extend(id3_text(b"TPE1", a));
    }
    v.extend(id3_text(b"TIT2", song));
    v
}

fn id3_chap(id: &str, start_ms: u32, end_ms: u32, title: &str) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(id.as_bytes());
    p.push(0);
    p.extend_from_slice(&start_ms.to_be_bytes());
    p.extend_from_slice(&end_ms.to_be_bytes());
    p.extend_from_slice(&u32::MAX.to_be_bytes()); // offsets en octets : non utilisés
    p.extend_from_slice(&u32::MAX.to_be_bytes());
    p.extend(title_frames(title));
    id3_frame(b"CHAP", &p)
}

fn id3_ctoc(ids: &[String]) -> Vec<u8> {
    let mut p = b"toc\0".to_vec();
    p.push(0x03); // racine (0x02) + chapitres ordonnés (0x01)
    p.push(ids.len() as u8);
    for id in ids {
        p.extend_from_slice(id.as_bytes());
        p.push(0);
    }
    p.extend(id3_text(b"TIT2", "Titres"));
    id3_frame(b"CTOC", &p)
}

fn build_id3(chapters: &[Chapter], sr: u64, h: &IcyHeaders) -> Vec<u8> {
    let to_ms = |s: u64| (s * 1000 / sr).min(u32::MAX as u64) as u32;
    let mut frames = Vec::new();
    if let Some(c) = chapters.first() {
        frames.extend(title_frames(c.title));
    }
    if let Some(n) = h.name.as_deref().filter(|s| !s.is_empty()) {
        frames.extend(id3_text(b"TRSN", n));
    }
    if let Some(g) = h.genre.as_deref().filter(|s| !s.is_empty()) {
        frames.extend(id3_text(b"TCON", g));
    }
    if let Some(u) = h.url.as_deref().filter(|s| !s.is_empty() && s.is_ascii()) {
        frames.extend(id3_frame(b"WORS", u.as_bytes()));
    }
    if chapters.len() > 1 {
        let chapters = &chapters[..chapters.len().min(255)]; // limite de CTOC
        let ids: Vec<String> = (0..chapters.len()).map(|k| format!("chp{k}")).collect();
        frames.extend(id3_ctoc(&ids));
        for (id, c) in ids.iter().zip(chapters) {
            frames.extend(id3_chap(id, to_ms(c.start), to_ms(c.end), c.title));
        }
    }
    if frames.is_empty() {
        return Vec::new();
    }
    const PADDING: usize = 512;
    let size = frames.len() + PADDING;
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    tag.extend_from_slice(&[
        ((size >> 21) & 0x7F) as u8,
        ((size >> 14) & 0x7F) as u8,
        ((size >> 7) & 0x7F) as u8,
        (size & 0x7F) as u8,
    ]);
    tag.extend(frames);
    tag.resize(tag.len() + PADDING, 0);
    tag
}

// ---------------------------------------------------------------- CUE

/// Les guillemets ne peuvent pas être échappés dans un .cue : on les remplace.
fn cue_str(s: &str) -> String {
    s.replace('"', "'").replace(['\r', '\n'], " ")
}

/// Horodatage CUE « mm:ss:ff » (75 images par seconde ; les minutes peuvent dépasser 99).
fn cue_time(samples: u64, sr: u64) -> String {
    let f = samples * 75 / sr;
    format!("{:02}:{:02}:{:02}", f / (75 * 60), (f / 75) % 60, f % 75)
}

/// Fichier .cue en UTF-8 avec BOM et fins de ligne CRLF. 99 pistes au maximum (norme CUE).
fn build_cue(mp3_name: &str, chapters: &[Chapter], sr: u64, h: &IcyHeaders) -> String {
    let mut c = String::from("\u{FEFF}");
    if let Some(g) = h.genre.as_deref().filter(|s| !s.is_empty()) {
        c += &format!("REM GENRE \"{}\"\r\n", cue_str(g));
    }
    if let Some(u) = h.url.as_deref().filter(|s| !s.is_empty()) {
        c += &format!("REM COMMENT \"{}\"\r\n", cue_str(u));
    }
    if let Some(n) = h.name.as_deref().filter(|s| !s.is_empty()) {
        c += &format!("PERFORMER \"{}\"\r\n", cue_str(n));
        c += &format!("TITLE \"{}\"\r\n", cue_str(n));
    }
    c += &format!("FILE \"{}\" MP3\r\n", cue_str(mp3_name));
    for (k, ch) in chapters.iter().take(99).enumerate() {
        let (artist, song) = split_title(ch.title);
        c += &format!("  TRACK {:02} AUDIO\r\n", k + 1);
        c += &format!("    TITLE \"{}\"\r\n", cue_str(song));
        if let Some(a) = artist {
            c += &format!("    PERFORMER \"{}\"\r\n", cue_str(a));
        }
        c += &format!("    INDEX 01 {}\r\n", cue_time(ch.start, sr));
    }
    c
}

// ---------------------------------------------------------------- programme

fn run(input: &Path, output: &Path, forced: Option<usize>) -> Result<(), String> {
    let data = fs::read(input).map_err(|e| format!("lecture de {} : {e}", input.display()))?;
    let (hlen, headers) = parse_http_headers(&data);
    let body = &data[hlen..];
    if hlen > 0 {
        println!("En-tête ICY/HTTP retiré : {hlen} octets");
    }
    if let Some(n) = &headers.name {
        println!("Station : {n}");
    }

    let metaint = forced.or(headers.metaint).or_else(|| detect_metaint(body));
    let (audio, tracks, removed) = match metaint {
        Some(m) => {
            let src = if forced.is_some() { "forcé" } else if headers.metaint.is_some() { "en-tête" } else { "détecté" };
            println!("icy-metaint : {m} ({src})");
            demux(body, m)
        }
        None => {
            println!("Aucun bloc de métadonnées détecté : flux traité comme audio pur (--metaint N pour forcer).");
            (body.to_vec(), Vec::new(), 0)
        }
    };
    println!("Métadonnées retirées : {removed} octets");

    let (frames, dropped, first, count, samples, index) = clean_frames(&audio);
    let first = first.ok_or_else(|| "aucune trame MPEG audio valide (le flux est peut-être en AAC ?)".to_string())?;
    let sr = first.sample_rate as u64;
    // Position exacte (en échantillons) de la première trame conservée à partir d'un offset audio.
    let samples_at = |off: usize| -> u64 {
        let k = index.partition_point(|&(o, _)| o < off);
        index.get(k).map_or(samples, |&(_, s)| s)
    };
    let secs = |s: u64| s as f64 / sr as f64;

    println!("Format : {}", first.describe());
    println!("Trames : {count}, durée ≈ {}", fmt_time(secs(samples)));
    println!("Octets écartés (trames coupées ou parasites) : {dropped}");

    // Un chapitre par titre ; le premier commence à 0.
    let chapters: Vec<Chapter> = tracks
        .iter()
        .enumerate()
        .map(|(k, t)| Chapter {
            start: if k == 0 { 0 } else { samples_at(t.audio_offset) },
            end: tracks.get(k + 1).map_or(samples, |n| samples_at(n.audio_offset)),
            title: t.title.as_str(),
        })
        .collect();

    if !chapters.is_empty() {
        println!("Titres :");
        for c in &chapters {
            println!("  [{} → {}] {}", fmt_time(secs(c.start)), fmt_time(secs(c.end)), c.title);
        }
    }

    // MP3 corrigé
    let mut out = build_id3(&chapters, sr, &headers);
    out.extend_from_slice(&frames);
    fs::write(output, &out).map_err(|e| format!("écriture de {} : {e}", output.display()))?;
    println!("Écrit : {} ({} octets)", output.display(), out.len());
    if chapters.len() > 1 {
        println!("  {} chapitres ID3 (CHAP/CTOC)", chapters.len().min(255));
    }

    // Index .cue : même nom que le MP3 de sortie, référence relative (même dossier)
    if chapters.is_empty() {
        println!("Aucun titre trouvé : pas de fichier .cue.");
    } else {
        let cue_path = output.with_extension("cue");
        let mp3_name = output
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| "nom de fichier de sortie invalide".to_string())?;
        let cue = build_cue(&mp3_name, &chapters, sr, &headers);
        fs::write(&cue_path, cue).map_err(|e| format!("écriture de {} : {e}", cue_path.display()))?;
        println!("Écrit : {} ({} pistes)", cue_path.display(), chapters.len().min(99));
        if chapters.len() > 99 {
            println!("  Attention : {} titres trouvés, le format CUE est limité à 99 pistes.", chapters.len());
        }
    }
    Ok(())
}

fn usage() {
    eprintln!("Usage : icyfix <entrée.mp3> [sortie.mp3] [--metaint N]");
    eprintln!("Écrit le MP3 corrigé et un fichier .cue du même nom à côté.");
}

fn main() -> ExitCode {
    let mut it = env::args().skip(1);
    let (mut input, mut output, mut metaint) = (None::<PathBuf>, None::<PathBuf>, None::<usize>);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                usage();
                return ExitCode::SUCCESS;
            }
            "--metaint" => match it.next().and_then(|v| v.parse().ok()).filter(|&m: &usize| m > 0) {
                Some(m) => metaint = Some(m),
                None => {
                    eprintln!("--metaint attend un entier > 0");
                    return ExitCode::FAILURE;
                }
            },
            _ if input.is_none() => input = Some(PathBuf::from(a)),
            _ if output.is_none() => output = Some(PathBuf::from(a)),
            _ => {
                usage();
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(input) = input else {
        usage();
        return ExitCode::FAILURE;
    };
    let output = output.unwrap_or_else(|| {
        let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "sortie".into());
        input.with_file_name(format!("{stem}_fixed.mp3"))
    });
    if output == input || output.with_extension("cue") == input {
        eprintln!("La sortie doit être différente de l'entrée.");
        return ExitCode::FAILURE;
    }
    match run(&input, &output, metaint) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Erreur : {e}");
            ExitCode::FAILURE
        }
    }
}