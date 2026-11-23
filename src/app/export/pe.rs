//! P6 Stage 2.2: what a Windows executable needs (its imported DLLs) and
//! which build it is, read from the file without running it.

/// The DLL names an executable imports (its import directory), in order.
pub fn imported_dlls(bytes: &[u8]) -> Result<Vec<String>, String> {
    let bad = |why: &str| Err(format!("not a valid Windows executable ({why})"));
    if bytes.len() < 0x40 || &bytes[0..2] != b"MZ" {
        return bad("no MZ header");
    }
    let pe = u32_at(bytes, 0x3c)? as usize;
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0".as_slice()) {
        return bad("no PE signature");
    }
    let coff = pe + 4;
    let sections = u16_at(bytes, coff + 2)? as usize;
    let optional_size = u16_at(bytes, coff + 16)? as usize;
    let optional = coff + 20;
    let (count_at, dirs_at) = match u16_at(bytes, optional)? {
        0x10b => (optional + 92, optional + 96),
        0x20b => (optional + 108, optional + 112),
        _ => return bad("unknown optional header"),
    };
    if u32_at(bytes, count_at)? < 2 {
        return Ok(Vec::new());
    }
    let import_rva = u32_at(bytes, dirs_at + 8)?;
    if import_rva == 0 {
        return Ok(Vec::new());
    }
    let table = optional + optional_size;
    let mut map = Vec::with_capacity(sections);
    for i in 0..sections.min(96) {
        let s = table + i * 40;
        let virtual_size = u32_at(bytes, s + 8)?;
        let virtual_address = u32_at(bytes, s + 12)?;
        let raw_size = u32_at(bytes, s + 16)?;
        let raw_pointer = u32_at(bytes, s + 20)?;
        map.push((virtual_address, virtual_size.max(raw_size), raw_pointer));
    }
    let offset = |rva: u32| -> Result<usize, String> {
        map.iter()
            .find(|(va, size, _)| rva >= *va && rva < va.saturating_add(*size))
            .map(|(va, _, raw)| (rva - va) as usize + *raw as usize)
            .ok_or_else(|| "not a valid Windows executable (bad import address)".to_string())
    };
    let mut names = Vec::new();
    let mut at = offset(import_rva)?;
    for _ in 0..4096 {
        let descriptor = bytes
            .get(at..at + 20)
            .ok_or("not a valid Windows executable (truncated imports)")?;
        if descriptor.iter().all(|b| *b == 0) {
            return Ok(names);
        }
        let name_at = offset(u32_at(bytes, at + 12)?)?;
        let name: Vec<u8> = bytes
            .get(name_at..)
            .unwrap_or_default()
            .iter()
            .take(260)
            .take_while(|b| **b != 0)
            .copied()
            .collect();
        names.push(String::from_utf8_lossy(&name).into_owned());
        at += 20;
    }
    bad("too many imports")
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| "not a valid Windows executable (truncated)".to_string())
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "not a valid Windows executable (truncated)".to_string())
}

/// DLLs every supported Windows (10 or later) provides. Anything else an
/// executable imports must be shipped beside it.
pub fn is_system_dll(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if name.starts_with("api-ms-win-") || name.starts_with("ext-ms-") {
        return true;
    }
    const SYSTEM: &[&str] = &[
        "advapi32.dll",
        "bcrypt.dll",
        "bcryptprimitives.dll",
        "cfgmgr32.dll",
        "combase.dll",
        "comctl32.dll",
        "comdlg32.dll",
        "crypt32.dll",
        "d2d1.dll",
        "d3d11.dll",
        "d3d12.dll",
        "d3dcompiler_47.dll",
        "dbghelp.dll",
        "dwmapi.dll",
        "dwrite.dll",
        "dxgi.dll",
        "gdi32.dll",
        "hid.dll",
        "imm32.dll",
        "iphlpapi.dll",
        "kernel32.dll",
        "kernelbase.dll",
        "mf.dll",
        "mfplat.dll",
        "mfreadwrite.dll",
        "msvcrt.dll",
        "ncrypt.dll",
        "netapi32.dll",
        "ntdll.dll",
        "ole32.dll",
        "oleaut32.dll",
        "opengl32.dll",
        "pdh.dll",
        "powrprof.dll",
        "propsys.dll",
        "psapi.dll",
        "secur32.dll",
        "setupapi.dll",
        "shcore.dll",
        "shell32.dll",
        "shlwapi.dll",
        "ucrtbase.dll",
        "user32.dll",
        "userenv.dll",
        "uxtheme.dll",
        "version.dll",
        "winhttp.dll",
        "winmm.dll",
        "winspool.drv",
        "wintrust.dll",
        "ws2_32.dll",
        "wsock32.dll",
    ];
    SYSTEM.contains(&name.as_str())
}

// ── Build profile marker ────────────────────────────────────────────────────
//
// Every Patchwork executable carries one marker naming its build profile.
// The exporter reads it from the runtime file it is given and refuses a
// debug build. The patterns are stored scrambled so the only plain copy of
// a marker in an executable is its own.

const KEY: u8 = 0xA5;
const MARKER_LEN: usize = 32;

const fn scramble(bytes: &[u8; MARKER_LEN]) -> [u8; MARKER_LEN] {
    let mut out = [0u8; MARKER_LEN];
    let mut i = 0;
    while i < MARKER_LEN {
        out[i] = bytes[i] ^ KEY;
        i += 1;
    }
    out
}

const RELEASE_X: [u8; MARKER_LEN] = scramble(b"\0patchwork-runtime-profile:rel\0\0");
const DEBUG_X: [u8; MARKER_LEN] = scramble(b"\0patchwork-runtime-profile:dbg\0\0");

/// This executable's marker (kept by [`touch_profile_marker`]).
#[used]
pub static RUNTIME_PROFILE_MARKER: [u8; MARKER_LEN] = if cfg!(debug_assertions) {
    scramble(&DEBUG_X)
} else {
    scramble(&RELEASE_X)
};

/// Keep the marker in the linked executable (called at startup).
pub fn touch_profile_marker() {
    std::hint::black_box(&RUNTIME_PROFILE_MARKER);
    std::hint::black_box(&BUILD_INFO);
}

/// The build profile an executable's bytes declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildProfile {
    Release,
    Debug,
}

/// The marker bytes for `profile` (for tests building executables).
pub fn profile_marker(profile: BuildProfile) -> Vec<u8> {
    let scrambled = match profile {
        BuildProfile::Release => RELEASE_X,
        BuildProfile::Debug => DEBUG_X,
    };
    scrambled.iter().map(|b| b ^ KEY).collect()
}

/// The profile marked in `bytes`, if exactly one kind is present.
pub fn build_profile(bytes: &[u8]) -> Result<BuildProfile, String> {
    let has = |profile| {
        let marker = profile_marker(profile);
        bytes.windows(MARKER_LEN).any(|w| w == marker.as_slice())
    };
    match (has(BuildProfile::Release), has(BuildProfile::Debug)) {
        (true, false) => Ok(BuildProfile::Release),
        (false, true) => Ok(BuildProfile::Debug),
        (false, false) => Err("it is not a Patchwork runtime (no build marker)".to_string()),
        (true, true) => Err("its build marker is ambiguous".to_string()),
    }
}

// ── Subsystem ───────────────────────────────────────────────────────────────

/// `IMAGE_SUBSYSTEM_WINDOWS_GUI`: starts without a console window.
pub const SUBSYSTEM_GUI: u16 = 2;
/// `IMAGE_SUBSYSTEM_WINDOWS_CUI`: a console program.
#[cfg(test)]
pub const SUBSYSTEM_CONSOLE: u16 = 3;

/// The Windows subsystem an executable is built for (read, never changed).
pub fn subsystem(bytes: &[u8]) -> Result<u16, String> {
    if bytes.len() < 0x40 || &bytes[0..2] != b"MZ" {
        return Err("not a valid Windows executable (no MZ header)".to_string());
    }
    let pe = u32_at(bytes, 0x3c)? as usize;
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0".as_slice()) {
        return Err("not a valid Windows executable (no PE signature)".to_string());
    }
    let optional = pe + 24;
    match u16_at(bytes, optional)? {
        // The field is at the same offset in PE32 and PE32+ headers.
        0x10b | 0x20b => u16_at(bytes, optional + 68),
        _ => Err("not a valid Windows executable (unknown optional header)".to_string()),
    }
}

// ── Build information ───────────────────────────────────────────────────────
//
// P6 Stage 2.3: every Patchwork executable also records which binary it is
// (`editor` or `player`) and the source it was built from (build.rs). The
// prefix is stored scrambled, like the profile markers, so the only plain
// copy in an executable is its own record.

const INFO_LEN: usize = 160;
const INFO_PREFIX_LEN: usize = 24;
const INFO_PREFIX_X: [u8; INFO_PREFIX_LEN] = {
    let plain = b"\0patchwork-build-info:v1";
    let mut out = [0u8; INFO_PREFIX_LEN];
    let mut i = 0;
    while i < INFO_PREFIX_LEN {
        out[i] = plain[i] ^ KEY;
        i += 1;
    }
    out
};

/// The source commit this binary was built from (`unknown` if not known).
pub const SOURCE_COMMIT: &str = env!("PATCHWORK_SOURCE_COMMIT");
/// `0`: built from that commit exactly; `1`: with local changes;
/// `unknown`.
pub const SOURCE_MODIFIED: &str = env!("PATCHWORK_SOURCE_MODIFIED");

const fn put(mut out: [u8; INFO_LEN], at: usize, text: &[u8]) -> ([u8; INFO_LEN], usize) {
    let mut i = 0;
    while i < text.len() && at + i < INFO_LEN - 1 {
        out[at + i] = text[i];
        i += 1;
    }
    (out, at + i)
}

const fn build_info_record(flavor: &str) -> [u8; INFO_LEN] {
    let mut prefix = [0u8; INFO_PREFIX_LEN];
    let mut i = 0;
    while i < INFO_PREFIX_LEN {
        prefix[i] = INFO_PREFIX_X[i] ^ KEY;
        i += 1;
    }
    let out = [0u8; INFO_LEN];
    let (out, at) = put(out, 0, &prefix);
    let (out, at) = put(out, at, b";flavor=");
    let (out, at) = put(out, at, flavor.as_bytes());
    let (out, at) = put(out, at, b";commit=");
    let (out, at) = put(out, at, SOURCE_COMMIT.as_bytes());
    let (out, at) = put(out, at, b";modified=");
    let (out, at) = put(out, at, SOURCE_MODIFIED.as_bytes());
    let (out, _) = put(out, at, b";");
    out
}

/// This executable's build information (kept by [`touch_profile_marker`]).
#[used]
pub static BUILD_INFO: [u8; INFO_LEN] = build_info_record(crate::BINARY.flavor());

/// What an executable's build information says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    /// `editor` or `player`.
    pub flavor: String,
    /// 40 hex digits, or `unknown`.
    pub commit: String,
    /// `Some(false)`: exactly that commit; `Some(true)`: local changes;
    /// `None`: not known.
    pub modified: Option<bool>,
}

impl BuildInfo {
    /// A commit that names the source exactly.
    pub fn exact_commit(&self) -> Option<&str> {
        (self.modified == Some(false)
            && self.commit.len() == 40
            && self.commit.chars().all(|c| c.is_ascii_hexdigit()))
        .then_some(self.commit.as_str())
    }
}

/// A build-information record (for tests building executables).
#[cfg(test)]
pub fn build_info_bytes(flavor: &str, commit: &str, modified: &str) -> Vec<u8> {
    let mut out: Vec<u8> = INFO_PREFIX_X.iter().map(|b| b ^ KEY).collect();
    out.extend_from_slice(
        format!(";flavor={flavor};commit={commit};modified={modified};").as_bytes(),
    );
    out.resize(INFO_LEN, 0);
    out
}

/// The build information recorded in `bytes` (exactly one record).
pub fn build_info(bytes: &[u8]) -> Result<BuildInfo, String> {
    let prefix: Vec<u8> = INFO_PREFIX_X.iter().map(|b| b ^ KEY).collect();
    let mut found = bytes
        .windows(INFO_PREFIX_LEN)
        .enumerate()
        .filter(|(_, w)| *w == prefix.as_slice())
        .map(|(at, _)| at);
    let at = found
        .next()
        .ok_or("it records no Patchwork build information (an older or foreign build)")?;
    if found.next().is_some() {
        return Err("its build information is ambiguous".to_string());
    }
    let record = &bytes[at + INFO_PREFIX_LEN..bytes.len().min(at + INFO_LEN)];
    let text: Vec<u8> = record.iter().take_while(|b| **b != 0).copied().collect();
    let text = String::from_utf8_lossy(&text);
    let field = |key: &str| {
        text.split(';')
            .find_map(|part| part.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .map(str::to_string)
    };
    let flavor = field("flavor").ok_or("its build information is incomplete")?;
    let commit = field("commit").ok_or("its build information is incomplete")?;
    let modified = match field("modified").as_deref() {
        Some("0") => Some(false),
        Some("1") => Some(true),
        _ => None,
    };
    Ok(BuildInfo {
        flavor,
        commit,
        modified,
    })
}
