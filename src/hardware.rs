use serialport::{SerialPortType, UsbPortInfo};
use std::path::PathBuf;
#[derive(Debug, Clone)]
pub struct HardwareDevice {
    pub port_name: String,
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub board_family: String,
}

fn identify_board(usb: &UsbPortInfo) -> String {
    match (usb.vid, usb.pid) {
        (0x303A, _) => "Espressif USB device".to_string(),
        (0x2E8A, _) => "Raspberry Pi USB device".to_string(),
        (0x10C4, 0xEA60) => "CP210x USB-to-serial adapter".to_string(),
        (0x1A86, 0x7523) => "CH340 USB-to-serial adapter".to_string(),
        _ => "Unknown USB serial device".to_string(),
    }
}

pub fn discover_devices() -> Result<Vec<HardwareDevice>, String> {
    #[cfg(test)]
    if let Some(devices) = ESPTOOL_TEST_DEVICES.with(|slot| slot.borrow().clone()) {
        return Ok(devices);
    }
    let ports = serialport::available_ports()
        .map_err(|e| format!("Failed to enumerate serial ports: {e}"))?;

    let mut devices = Vec::new();

    for port in ports {
        if let SerialPortType::UsbPort(usb) = port.port_type {
            devices.push(HardwareDevice {
                port_name: port.port_name,
                vid: usb.vid,
                pid: usb.pid,
                manufacturer: usb.manufacturer.clone(),
                product: usb.product.clone(),
                board_family: identify_board(&usb),
            });
        }
    }

    devices.sort_by(|a, b| a.port_name.cmp(&b.port_name));

    Ok(devices)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Esp32FirmwareLayout {
    Application,
    Merged,
    MultiFile,
}

#[derive(Debug, Clone)]
pub struct Esp32FlashConfig {
    pub layout: Esp32FirmwareLayout,
    pub flash_address: u32,
    pub chip: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Esp32FirmwareComponent {
    Bootloader,
    PartitionTable,
    Application,
    Data,
}

#[derive(Debug, Clone)]
pub struct Esp32FirmwareSegment {
    pub address: u32,
    pub path: PathBuf,
    pub component: Esp32FirmwareComponent,
}
#[derive(Debug, Clone)]
pub struct Esp32FirmwareManifest {
    pub chip: String,
    pub segments: Vec<Esp32FirmwareSegment>,
}
pub fn validate_esp32_flash_capacity(
    manifest: &Esp32FirmwareManifest,
    flash_size: u64,
) -> Result<(), String> {
    if flash_size == 0 {
        return Err("ESP32 flash capacity is unknown".to_string());
    }

    for segment in &manifest.segments {
        let size = std::fs::metadata(&segment.path)
            .map_err(|e| format!("Cannot read firmware file metadata: {e}"))?
            .len();

        let end = u64::from(segment.address)
            .checked_add(size)
            .ok_or("ESP32 firmware address overflow")?;

        if end > flash_size {
            return Err(format!(
                "ESP32 firmware exceeds flash capacity: \
                 file {} ends at 0x{:X}, \
                 but flash capacity is 0x{:X}",
                segment.path.display(),
                end,
                flash_size
            ));
        }
    }
    Ok(())
}
pub fn validate_esp32_segment_content(segment: &Esp32FirmwareSegment) -> Result<(), String> {
    match segment.component {
        Esp32FirmwareComponent::Bootloader | Esp32FirmwareComponent::Application => {
            validate_esp32_image(&segment.path)
        }

        Esp32FirmwareComponent::PartitionTable => Err("ESP32 partition-table content validation \
                 is not yet implemented"
            .to_string()),

        Esp32FirmwareComponent::Data => Err("ESP32 data-component validation \
                 is not yet implemented"
            .to_string()),
    }
}
// Centralized esptool invocation. Tests can inject a runner without launching Python.
fn run_esptool_with<F>(args: &[&std::ffi::OsStr], runner: F) -> Result<std::process::Output, String>
where
    F: FnOnce(&[&std::ffi::OsStr]) -> Result<std::process::Output, String>,
{
    runner(args)
}

// Test-only, thread-local simulation. Production builds never use these hooks.
#[cfg(test)]
thread_local! {
    static ESPTOOL_TEST_RUNNER: std::cell::RefCell<Option<Box<
        dyn FnMut(&[&std::ffi::OsStr]) -> Result<std::process::Output, String>
    >>> = std::cell::RefCell::new(None);
    static ESPTOOL_TEST_DEVICES: std::cell::RefCell<Option<Vec<HardwareDevice>>> =
        std::cell::RefCell::new(None);
    static ESPTOOL_TEST_PARTITION_OFFSET: std::cell::Cell<Option<u32>> =
        std::cell::Cell::new(None);
}

fn run_esptool(args: &[&std::ffi::OsStr]) -> Result<std::process::Output, String> {
    #[cfg(test)]
    {
        let mocked = ESPTOOL_TEST_RUNNER.with(|slot| {
            let mut slot = slot.borrow_mut();
            slot.as_mut().map(|runner| runner(args))
        });
        if let Some(result) = mocked {
            return result;
        }
    }
    run_esptool_with(args, |args| {
        let mut command = std::process::Command::new("python");
        command.arg("-m").arg("esptool");
        command.args(args);
        command
            .output()
            .map_err(|error| format!("Failed to start esptool: {error}"))
    })
}

pub fn validate_esp32_firmware_package(
    manifest: &Esp32FirmwareManifest,
    flash_size: u64,
) -> Result<(), String> {
    // Validate chip, files, addresses, and overlapping regions.
    validate_esp32_manifest(manifest)?;

    // Verify that all segments fit within flash capacity.
    validate_esp32_flash_capacity(manifest, flash_size)?;

    // Validate the actual contents of every firmware segment.
    for segment in &manifest.segments {
        validate_esp32_segment_content(segment)?;
    }

    Ok(())
}

pub fn validate_esp32_manifest(manifest: &Esp32FirmwareManifest) -> Result<(), String> {
    if manifest.chip.trim().is_empty() {
        return Err("ESP32 manifest chip cannot be empty".to_string());
    }
    // Reuse existing ESP32 chip validation.
    let chip_config = Esp32FlashConfig {
        layout: Esp32FirmwareLayout::Application,
        flash_address: 0x10000,
        chip: manifest.chip.clone(),
    };

    validate_esp32_flash_config(&chip_config)?;

    if manifest.segments.is_empty() {
        return Err("ESP32 manifest contains no firmware files".to_string());
    }

    let mut addresses = std::collections::HashSet::new();

    for segment in &manifest.segments {
        if segment.address % 0x1000 != 0 {
            return Err(format!(
                "ESP32 flash address 0x{:X} is not 4KB aligned",
                segment.address
            ));
        }

        if !addresses.insert(segment.address) {
            return Err(format!(
                "Duplicate ESP32 flash address: 0x{:X}",
                segment.address
            ));
        }

        if !segment.path.is_file() {
            return Err(format!(
                "ESP32 firmware file not found: {}",
                segment.path.display()
            ));
        }
    }
    // Calculate the flash region occupied by each firmware file.
    let mut regions = Vec::new();

    for segment in &manifest.segments {
        let size = std::fs::metadata(&segment.path)
            .map_err(|e| format!("Cannot read firmware file metadata: {e}"))?
            .len();

        if size == 0 {
            return Err(format!(
                "ESP32 firmware file is empty: {}",
                segment.path.display()
            ));
        }

        let start = u64::from(segment.address);
        let end = start + size;

        if end > 0x1_0000_0000 {
            return Err(format!(
                "ESP32 firmware region exceeds 32-bit address space: {}",
                segment.path.display()
            ));
        }

        regions.push((start, end));
    }

    regions.sort_by_key(|region| region.0);

    for pair in regions.windows(2) {
        let previous = pair[0];
        let next = pair[1];

        if previous.1 > next.0 {
            return Err(format!(
                "Overlapping ESP32 firmware regions: \
                 0x{:X}-0x{:X} and 0x{:X}-0x{:X}",
                previous.0, previous.1, next.0, next.1
            ));
        }
    }
    Ok(())
}
// P5: Supported firmware targets
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareTarget {
    Esp32,
    RaspberryPiPico,
}

// P5: Firmware flashing request
#[derive(Debug, Clone)]
pub struct FlashRequest {
    pub target: FirmwareTarget,
    pub firmware_path: std::path::PathBuf,
    pub port_name: Option<String>,
}

// P5: Firmware flashing status
#[derive(Debug, Clone)]
pub enum FlashStatus {
    Idle,
    Validating,
    Flashing,
    Completed,
    Failed(String),
}
// P5: Validate firmware before flashing.
pub fn validate_firmware(request: &FlashRequest) -> Result<(), String> {
    let path = &request.firmware_path;

    if !path.is_file() {
        return Err(format!("Firmware file not found: {}", path.display()));
    }

    let expected_extension = match request.target {
        FirmwareTarget::Esp32 => "bin",
        FirmwareTarget::RaspberryPiPico => "uf2",
    };

    let actual_extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");

    if !actual_extension.eq_ignore_ascii_case(expected_extension) {
        return Err(format!(
            "Invalid firmware format. Expected .{expected_extension}"
        ));
    }

    let metadata =
        std::fs::metadata(path).map_err(|e| format!("Cannot read firmware metadata: {e}"))?;

    if metadata.len() == 0 {
        return Err("Firmware file is empty".to_string());
    }

    // Validate UF2 structure for Raspberry Pi Pico firmware.
    if request.target == FirmwareTarget::RaspberryPiPico {
        validate_uf2_header(path)?;
    }

    Ok(())
}

// P5: Validate the basic UF2 firmware structure.
pub fn validate_uf2_header(path: &std::path::Path) -> Result<(), String> {
    use std::io::Read;

    let mut file =
        std::fs::File::open(path).map_err(|e| format!("Cannot open UF2 firmware: {e}"))?;

    let mut header = [0u8; 32];

    file.read_exact(&mut header)
        .map_err(|e| format!("Invalid UF2 header: {e}"))?;

    let magic_start0 = u32::from_le_bytes(header[0..4].try_into().unwrap());

    let magic_start1 = u32::from_le_bytes(header[4..8].try_into().unwrap());

    if magic_start0 != 0x0A324655 || magic_start1 != 0x9E5D5157 {
        return Err("Invalid UF2 magic numbers".to_string());
    }

    let file_size = std::fs::metadata(path)
        .map_err(|e| format!("Cannot read UF2 file size: {e}"))?
        .len();

    if file_size == 0 || file_size % 512 != 0 {
        return Err("Invalid UF2 size: file must contain complete 512-byte blocks".to_string());
    }

    // Verify the ending magic number of every UF2 block.
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("Cannot reopen UF2 firmware: {e}"))?;

    let mut block = [0u8; 512];

    for block_index in 0..(file_size / 512) {
        file.read_exact(&mut block)
            .map_err(|e| format!("Cannot read UF2 block {block_index}: {e}"))?;

        // Validate the starting magic numbers of every UF2 block.
        let block_magic0 = u32::from_le_bytes(block[0..4].try_into().unwrap());

        let block_magic1 = u32::from_le_bytes(block[4..8].try_into().unwrap());

        if block_magic0 != 0x0A324655 || block_magic1 != 0x9E5D5157 {
            return Err(format!("Invalid UF2 starting magic in block {block_index}"));
        }
        let end_magic = u32::from_le_bytes(block[508..512].try_into().unwrap());

        if end_magic != 0x0AB16F30 {
            return Err(format!("Invalid UF2 ending magic in block {block_index}"));
        }

        // Validate payload size.
        let payload_size = u32::from_le_bytes(block[16..20].try_into().unwrap());

        if payload_size == 0 || payload_size > 476 {
            return Err(format!(
                "Invalid UF2 payload size {payload_size} in block {block_index}"
            ));
        }

        // Validate block number and total block count.
        let block_number = u32::from_le_bytes(block[20..24].try_into().unwrap());

        let total_blocks = u32::from_le_bytes(block[24..28].try_into().unwrap());

        let actual_total_blocks = file_size / 512;

        if total_blocks == 0 || u64::from(total_blocks) != actual_total_blocks {
            return Err(format!(
                "Invalid UF2 total block count in block {block_index}: \
                 declared {total_blocks}, actual {actual_total_blocks}"
            ));
        }

        if u64::from(block_number) != block_index {
            return Err(format!(
                "Invalid UF2 block number: expected {block_index}, \
                 found {block_number}"
            ));
        }
    }

    Ok(())
}
// P5: Read the target family ID from a UF2 firmware file.
// P5: Read and verify the family ID across all UF2 blocks.
pub fn read_uf2_family_id(path: &std::path::Path) -> Result<Option<u32>, String> {
    use std::io::Read;

    validate_uf2_header(path)?;

    let mut file =
        std::fs::File::open(path).map_err(|e| format!("Cannot open UF2 firmware: {e}"))?;

    let mut block = [0u8; 512];
    let mut expected_family: Option<Option<u32>> = None;
    let mut block_index = 0usize;

    loop {
        match file.read_exact(&mut block) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("Cannot read UF2 block: {e}")),
        }

        let flags = u32::from_le_bytes(block[8..12].try_into().unwrap());

        let family = if flags & 0x00002000 != 0 {
            Some(u32::from_le_bytes(block[28..32].try_into().unwrap()))
        } else {
            None
        };

        if let Some(expected) = expected_family {
            if family != expected {
                return Err(format!("Inconsistent UF2 family ID in block {block_index}"));
            }
        } else {
            expected_family = Some(family);
        }

        block_index += 1;
    }

    Ok(expected_family.flatten())
}
// P5: Check whether UF2 firmware matches the selected Pico board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PicoBoard {
    Rp2040,
    Rp2350,
}

pub fn validate_pico_compatibility(path: &std::path::Path, board: PicoBoard) -> Result<(), String> {
    let family_id = read_uf2_family_id(path)?;

    match (board, family_id) {
        (PicoBoard::Rp2040, Some(0xE48BFF56)) => Ok(()),

        // RP2350 ARM Secure UF2 family.
        (PicoBoard::Rp2350, Some(0xE48BFF59)) => Ok(()),

        (_, None) => Err("UF2 firmware does not declare a family ID; \
             board compatibility cannot be verified"
            .to_string()),

        (_, Some(id)) => Err(format!(
            "UF2 firmware family 0x{id:08X} is not compatible \
             with selected board {board:?}"
        )),
    }
}
// P5: Discover possible Raspberry Pi Pico BOOTSEL drives on Windows.
#[cfg(target_os = "windows")]
pub fn discover_pico_bootloader_drives() -> Vec<std::path::PathBuf> {
    let mut drives = Vec::new();

    for letter in b'A'..=b'Z' {
        let root = std::path::PathBuf::from(format!("{}:\\", letter as char));

        if !root.exists() {
            continue;
        }

        // Pico BOOTSEL drives expose this information file.
        let info_path = root.join("INFO_UF2.TXT");

        if info_path.is_file() {
            drives.push(root);
        }
    }

    drives
}
// P5: Identify the Pico board from its BOOTSEL information file.
pub fn identify_pico_bootloader(info_path: &std::path::Path) -> Result<PicoBoard, String> {
    let content =
        std::fs::read_to_string(info_path).map_err(|e| format!("Cannot read INFO_UF2.TXT: {e}"))?;

    let normalized = content.to_ascii_lowercase();
    let is_rp2350 = normalized.contains("rp2350");

    let is_rp2040 = normalized.contains("rp2040") || normalized.contains("board-id: rpi-rp2");

    if is_rp2040 && is_rp2350 {
        return Err(
            "Conflicting Pico bootloader identifiers: both RP2040 and RP2350 detected".to_string(),
        );
    }

    if is_rp2350 {
        Ok(PicoBoard::Rp2350)
    } else if is_rp2040 {
        Ok(PicoBoard::Rp2040)
    } else {
        Err("Unknown Pico bootloader: RP2040/RP2350 not identified".to_string())
    }
}
// P5: Verify that UF2 firmware matches the detected Pico bootloader.
pub fn validate_pico_flash_target(
    firmware_path: &std::path::Path,
    bootloader_info_path: &std::path::Path,
) -> Result<PicoBoard, String> {
    // Identify the board from its bootloader metadata.
    let board = identify_pico_bootloader(bootloader_info_path)?;

    // Verify that the firmware matches that board.
    validate_pico_compatibility(firmware_path, board)?;

    Ok(board)
}
// P5: An identified Pico bootloader drive.
#[derive(Debug, Clone)]
pub struct PicoBootloaderDevice {
    pub drive_path: std::path::PathBuf,
    pub board: PicoBoard,
}
#[cfg(target_os = "windows")]
fn is_windows_removable_drive(path: &std::path::Path) -> bool {
    use std::os::windows::ffi::OsStrExt;

    if !is_windows_drive_root(path) {
        return false;
    }

    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDriveTypeW(root_path: *const u16) -> u32;
    }

    const DRIVE_REMOVABLE: u32 = 2;

    unsafe { GetDriveTypeW(wide_path.as_ptr()) == DRIVE_REMOVABLE }
}
#[cfg(target_os = "windows")]
fn is_windows_drive_root(path: &std::path::Path) -> bool {
    use std::path::Component;

    let mut components = path.components();

    matches!(components.next(), Some(Component::Prefix(_)))
        && matches!(components.next(), Some(Component::RootDir))
        && components.next().is_none()
}
// P5: Discover and identify Pico BOOTSEL drives on Windows.
#[cfg(target_os = "windows")]
pub fn discover_identified_pico_drives() -> Vec<PicoBootloaderDevice> {
    let mut devices = Vec::new();

    for drive_path in discover_pico_bootloader_drives() {
        let info_path = drive_path.join("INFO_UF2.TXT");
        if !is_windows_drive_root(&drive_path) || !is_windows_removable_drive(&drive_path) {
            continue;
        }
        if let Ok(board) = identify_pico_bootloader(&info_path) {
            devices.push(PicoBootloaderDevice { drive_path, board });
        }
    }

    devices
}
// P5: Validate a selected firmware and hardware target
// before any flashing operation is permitted.
pub fn validate_flash_target(
    request: &FlashRequest,
    pico_drive: Option<&std::path::Path>,
) -> Result<(), String> {
    // First validate the firmware file.
    validate_firmware(request)?;

    match request.target {
        FirmwareTarget::Esp32 => {
            if pico_drive.is_some() {
                return Err("ESP32 firmware cannot target a Pico BOOTSEL drive.".to_string());
            }

            let port = request
                .port_name
                .as_deref()
                .ok_or("No ESP32 serial port selected.")?;

            // Confirm that the selected port is a detected ESP32.
            let devices = discover_devices()?;

            let device = devices
                .iter()
                .find(|device| device.port_name == port)
                .ok_or("Selected serial device is not connected.")?;

            if !device.board_family.to_ascii_lowercase().contains("esp32") {
                return Err("Selected serial device is not identified as ESP32.".to_string());
            }

            Ok(())
        }

        FirmwareTarget::RaspberryPiPico => {
            if request.port_name.is_some() {
                return Err(
                    "Pico UF2 firmware requires a BOOTSEL drive, not a serial port.".to_string(),
                );
            }

            let drive = pico_drive.ok_or("No Pico BOOTSEL drive selected.")?;

            let info_path = drive.join("INFO_UF2.TXT");

            validate_pico_flash_target(&request.firmware_path, &info_path)?;

            Ok(())
        }
    }
}

pub fn validate_esp32_image(firmware_path: &std::path::Path) -> Result<(), String> {
    use std::io::Read;

    validate_firmware(&FlashRequest {
        target: FirmwareTarget::Esp32,
        firmware_path: firmware_path.to_path_buf(),
        port_name: None,
    })?;

    let mut file = std::fs::File::open(firmware_path)
        .map_err(|e| format!("Cannot open ESP32 firmware: {e}"))?;

    let mut header = [0u8; 8];

    file.read_exact(&mut header)
        .map_err(|e| format!("Cannot read ESP32 image header: {e}"))?;

    if header[0] != 0xE9 {
        return Err("Invalid ESP32 firmware: missing image magic byte 0xE9".to_string());
    }

    if header[1] == 0 || header[1] > 16 {
        return Err("Invalid ESP32 firmware: invalid segment count".to_string());
    }
    let output = run_esptool(&[
        std::ffi::OsStr::new("image-info"),
        firmware_path.as_os_str(),
    ])?;

    if !output.status.success() {
        return Err(format!(
            "ESP32 firmware image validation failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}
pub fn validate_esp32_firmware_chip(
    firmware_path: &std::path::Path,
    selected_chip: &str,
) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};

    let expected_id: u16 = match selected_chip {
        "esp32" => 0,
        "esp32s2" => 2,
        "esp32s3" => 9,
        "esp32c2" => 12,
        "esp32c3" => 5,
        "esp32c5" => 23,
        "esp32c6" => 13,
        "esp32c61" => 20,
        "esp32h2" => 16,
        "esp32h4" => 28,
        "esp32p4" => 18,
        "esp32s31" => 32,
        _ => return Err(format!("Unsupported ESP32 chip: {selected_chip}")),
    };

    let mut file =
        std::fs::File::open(firmware_path).map_err(|e| format!("Cannot open firmware: {e}"))?;

    file.seek(SeekFrom::Start(12))
        .map_err(|e| format!("Cannot seek firmware header: {e}"))?;

    let mut bytes = [0u8; 2];
    file.read_exact(&mut bytes)
        .map_err(|e| format!("Cannot read firmware chip ID: {e}"))?;

    let firmware_chip_id = u16::from_le_bytes(bytes);

    if firmware_chip_id != expected_id {
        return Err(format!(
            "Firmware chip mismatch: image ID {firmware_chip_id}, selected chip {selected_chip} (ID {expected_id})"
        ));
    }

    Ok(())
}
pub fn validate_esp32_flash_config(config: &Esp32FlashConfig) -> Result<(), String> {
    if config.chip.trim().is_empty() {
        return Err("ESP32 chip type must be specified".to_string());
    }
    const SUPPORTED_CHIPS: &[&str] = &[
        "esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61",
        "esp32h2", "esp32h4", "esp32p4", "esp32s31",
    ];

    if !SUPPORTED_CHIPS.contains(&config.chip.as_str()) {
        return Err(format!("Unsupported ESP32 chip type: {}", config.chip));
    }

    match config.layout {
        Esp32FirmwareLayout::Application => {
            validate_esp32_flash_address(config.flash_address)?;
        }
        Esp32FirmwareLayout::MultiFile => {
            return Err("Multi-file ESP32 firmware flashing is not yet supported. \
         A firmware manifest and individual flash addresses are required."
                .to_string());
        }

        Esp32FirmwareLayout::Merged => {
            return Err("Merged ESP32 firmware flashing is not yet supported. \
         Use a validated application image until merged-image \
         support is implemented."
                .to_string());
        }
    }

    Ok(())
}
pub fn validate_esp32_flash_address(flash_address: u32) -> Result<(), String> {
    if flash_address == 0 {
        return Err("ESP32 flash address 0x0 requires explicit merged-image support".to_string());
    }
    if flash_address < 0x10000 {
        return Err("ESP32 application flash address must be at least 0x10000. \
         Lower addresses may contain bootloader or partition data."
            .to_string());
    }
    if flash_address % 0x1000 != 0 {
        return Err("ESP32 flash address must be aligned to 0x1000 (4 KB)".to_string());
    }

    Ok(())
}
fn parse_esp32_chip_model(output: &str) -> Result<String, String> {
    for line in output.lines() {
        let line = line.trim();

        let Some((_, description)) = line.split_once("Chip is ") else {
            continue;
        };

        let model = description
            .split(|c: char| c == '(' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .replace('-', "");

        const SUPPORTED: &[&str] = &[
            "esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61",
            "esp32h2", "esp32h4", "esp32p4", "esp32s31",
        ];

        if SUPPORTED.contains(&model.as_str()) {
            return Ok(model);
        }

        return Err(format!("Unsupported ESP32 chip model: {description}"));
    }

    Err("Could not identify ESP32 chip model from esptool output".to_string())
}
pub fn detect_esp32_chip(port_name: &str) -> Result<String, String> {
    if port_name.trim().is_empty() {
        return Err("ESP32 serial port is not selected".to_string());
    }

    let output = run_esptool(&[
        std::ffi::OsStr::new("--port"),
        std::ffi::OsStr::new(port_name),
        std::ffi::OsStr::new("chip-id"),
    ])?;

    if !output.status.success() {
        return Err(format!(
            "ESP32 chip detection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_esp32_chip_model(&stdout)
}
fn validate_esp32_chip_match(detected_chip: &str, selected_chip: &str) -> Result<(), String> {
    if detected_chip.eq_ignore_ascii_case(selected_chip) {
        return Ok(());
    }

    Err(format!(
        "ESP32 chip mismatch: connected device is {}, but firmware configuration specifies {}",
        detected_chip, selected_chip
    ))
}
fn parse_esp32_flash_size(output: &str) -> Result<u64, String> {
    for line in output.lines() {
        if let Some((_, value)) = line.split_once("Detected flash size:") {
            let value = value.trim().to_ascii_uppercase();

            let (number, multiplier) = if let Some(n) = value.strip_suffix("MB") {
                (n.trim(), 1024_u64 * 1024)
            } else if let Some(n) = value.strip_suffix("KB") {
                (n.trim(), 1024_u64)
            } else {
                continue;
            };

            let size = number
                .parse::<u64>()
                .map_err(|_| "Invalid ESP32 flash size".to_string())?;

            return size
                .checked_mul(multiplier)
                .filter(|size| *size > 0)
                .ok_or_else(|| "Invalid ESP32 flash capacity".to_string());
        }
    }

    Err("Could not determine ESP32 flash capacity".to_string())
}
pub fn detect_esp32_flash_size(port_name: &str, chip: &str) -> Result<u64, String> {
    if port_name.trim().is_empty() {
        return Err("ESP32 serial port is not selected".to_string());
    }

    let output = run_esptool(&[
        std::ffi::OsStr::new("--chip"),
        std::ffi::OsStr::new(chip),
        std::ffi::OsStr::new("--port"),
        std::ffi::OsStr::new(port_name),
        std::ffi::OsStr::new("flash-id"),
    ])?;

    if !output.status.success() {
        return Err(format!(
            "ESP32 flash detection failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_esp32_flash_size(&stdout)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Esp32Partition {
    pub partition_type: u8,
    pub subtype: u8,
    pub offset: u32,
    pub size: u32,
    pub label: String,
}

impl Esp32Partition {
    pub fn is_application(&self) -> bool {
        self.partition_type == 0x00
    }
}

fn validate_esp32_partition_overlaps(partitions: &[Esp32Partition]) -> Result<(), String> {
    for (i, first) in partitions.iter().enumerate() {
        let first_start = u64::from(first.offset);
        let first_end = first_start + u64::from(first.size);

        for second in partitions.iter().skip(i + 1) {
            let second_start = u64::from(second.offset);
            let second_end = second_start + u64::from(second.size);

            if first_start < second_end && second_start < first_end {
                return Err(format!(
                    "ESP32 partitions overlap: '{}' and '{}'",
                    first.label, second.label
                ));
            }
        }
    }

    Ok(())
}
fn parse_esp32_partition_table(data: &[u8]) -> Result<Vec<Esp32Partition>, String> {
    use md5::{Digest, Md5};

    const ENTRY_SIZE: usize = 32;
    const PARTITION_MAGIC: u16 = 0x50AA;
    const MD5_MAGIC: u16 = 0xEBEB;

    if data.is_empty() || data.len() > 0x1000 || data.len() % ENTRY_SIZE != 0 {
        return Err("Invalid ESP32 partition table length".to_string());
    }

    let mut partitions = Vec::new();
    let mut found_end = false;
    let mut found_md5 = false;

    for (index, entry) in data.chunks_exact(ENTRY_SIZE).enumerate() {
        if entry.iter().all(|byte| *byte == 0xFF) {
            found_end = true;
            break;
        }

        let magic = u16::from_le_bytes([entry[0], entry[1]]);
        if magic == MD5_MAGIC {
            if found_md5 || !entry[2..16].iter().all(|byte| *byte == 0xFF) {
                return Err("Malformed ESP32 partition-table MD5 entry".to_string());
            }
            let digest = Md5::digest(&data[..index * ENTRY_SIZE]);
            if digest.as_slice() != &entry[16..32] {
                return Err("ESP32 partition-table MD5 checksum mismatch".to_string());
            }
            found_md5 = true;
            continue;
        }

        if found_md5 {
            return Err("Partition entry after ESP32 MD5 checksum".to_string());
        }
        if magic != PARTITION_MAGIC {
            return Err("Invalid ESP32 partition entry magic".to_string());
        }

        let offset = u32::from_le_bytes(entry[4..8].try_into().unwrap());
        let size = u32::from_le_bytes(entry[8..12].try_into().unwrap());
        if size == 0 || offset.checked_add(size).is_none() {
            return Err("Invalid ESP32 partition boundaries".to_string());
        }

        let label_bytes = &entry[12..28];
        let label_end = label_bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(label_bytes.len());
        let label = std::str::from_utf8(&label_bytes[..label_end])
            .map_err(|_| "Invalid ESP32 partition label encoding".to_string())?
            .to_string();

        partitions.push(Esp32Partition {
            partition_type: entry[2],
            subtype: entry[3],
            offset,
            size,
            label,
        });
    }

    if !found_end || partitions.is_empty() {
        return Err("Incomplete ESP32 partition table".to_string());
    }
    validate_esp32_partition_overlaps(&partitions)?;
    Ok(partitions)
}

/// Read a partition table from a caller-supplied flash offset. This operation
/// reads only; it never erases or writes the connected ESP32.
pub fn read_esp32_partition_table(
    port_name: &str,
    chip: &str,
    partition_table_offset: u32,
) -> Result<Vec<Esp32Partition>, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static READ_COUNTER: AtomicU64 = AtomicU64::new(0);

    if port_name.trim().is_empty() || chip.trim().is_empty() {
        return Err("ESP32 port and chip must be specified".to_string());
    }
    if partition_table_offset % 0x1000 != 0 {
        return Err("ESP32 partition-table offset must be 0x1000-aligned".to_string());
    }

    let filename = format!(
        "patchwork-esp32-partitions-{}-{}.bin",
        std::process::id(),
        READ_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let output_path = std::env::temp_dir().join(filename);

    // Use create_new so we never overwrite an existing temporary file.
    let temp_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|e| format!("Cannot reserve ESP32 partition-table file: {e}"))?;
    drop(temp_file);

    let result = (|| {
        let offset_arg = format!("0x{partition_table_offset:X}");
        let output = run_esptool(&[
            std::ffi::OsStr::new("--chip"),
            std::ffi::OsStr::new(chip),
            std::ffi::OsStr::new("--port"),
            std::ffi::OsStr::new(port_name),
            std::ffi::OsStr::new("read-flash"),
            std::ffi::OsStr::new(&offset_arg),
            std::ffi::OsStr::new("0x1000"),
            output_path.as_os_str(),
        ])?;

        if !output.status.success() {
            return Err(format!(
                "ESP32 partition-table read failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let bytes = std::fs::read(&output_path)
            .map_err(|e| format!("Cannot read downloaded ESP32 partition table: {e}"))?;
        if bytes.len() != 0x1000 {
            return Err(format!(
                "ESP32 partition-table read returned {} bytes, expected 4096",
                bytes.len()
            ));
        }
        // Fail closed: a table without an MD5 entry has no verified integrity.
        let has_md5 = bytes
            .chunks_exact(32)
            .take_while(|entry| !entry.iter().all(|byte| *byte == 0xFF))
            .any(|entry| entry[0] == 0xEB && entry[1] == 0xEB);
        if !has_md5 {
            return Err("ESP32 partition table has no MD5 checksum; flashing blocked".to_string());
        }
        parse_esp32_partition_table(&bytes)
    })();

    let _ = std::fs::remove_file(&output_path);
    result
}

pub fn validate_esp32_application_partition(
    flash_address: u32,
    firmware_size: u64,
    partition_address: u32,
    partition_size: u64,
) -> Result<(), String> {
    if firmware_size == 0 || partition_size == 0 {
        return Err("Invalid ESP32 firmware or partition size".to_string());
    }

    let partition_start = u64::from(partition_address);

    let partition_end = partition_start
        .checked_add(partition_size)
        .ok_or("ESP32 partition address overflow")?;

    let firmware_start = u64::from(flash_address);

    let firmware_end = firmware_start
        .checked_add(firmware_size)
        .ok_or("ESP32 firmware address overflow")?;

    if firmware_start < partition_start || firmware_end > partition_end {
        return Err(format!(
            "ESP32 firmware does not fit within the application partition: \
             firmware 0x{:X}-0x{:X}, partition 0x{:X}-0x{:X}",
            firmware_start, firmware_end, partition_start, partition_end
        ));
    }

    Ok(())
}
// Application-only flashing is supported for a single, verified factory slot.
// OTA slots require additional boot-selection and rollback handling.
fn validate_esp32_factory_flash(
    partitions: &[Esp32Partition],
    flash_address: u32,
    firmware_size: u64,
    partition_table_offset: u32,
    flash_size: u64,
) -> Result<(), String> {
    if partitions
        .iter()
        .any(|p| p.is_application() && p.subtype != 0x00)
    {
        return Err(
            "ESP32 OTA or non-factory application partitions are not supported for flashing"
                .to_string(),
        );
    }
    let factories: Vec<_> = partitions
        .iter()
        .filter(|p| p.is_application() && p.subtype == 0x00)
        .collect();
    if factories.len() != 1 {
        return Err("ESP32 requires exactly one factory application partition".to_string());
    }
    let factory = factories[0];
    if flash_address != factory.offset {
        return Err(format!(
            "ESP32 flash address 0x{flash_address:X} must equal factory partition start 0x{:X}",
            factory.offset
        ));
    }
    if u64::from(partition_table_offset) + 0x1000 > u64::from(factory.offset) {
        return Err("ESP32 partition table overlaps the factory application partition".to_string());
    }
    for partition in partitions {
        let end = u64::from(partition.offset) + u64::from(partition.size);
        if end > flash_size {
            return Err(format!(
                "ESP32 partition '{}' exceeds physical flash capacity",
                partition.label
            ));
        }
    }
    validate_esp32_application_partition(
        flash_address,
        firmware_size,
        factory.offset,
        u64::from(factory.size),
    )
}

fn configured_esp32_partition_offset() -> Result<u32, String> {
    #[cfg(test)]
    if let Some(offset) = ESPTOOL_TEST_PARTITION_OFFSET.with(|slot| slot.get()) {
        return Ok(offset);
    }
    let text = std::env::var("PATCHWORK_ESP32_PARTITION_TABLE_OFFSET")
        .map_err(|_| "ESP32 flashing blocked: set PATCHWORK_ESP32_PARTITION_TABLE_OFFSET to the verified partition-table address".to_string())?;
    u32::from_str_radix(
        text.trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X"),
        16,
    )
    .map_err(|_| "Invalid ESP32 partition-table offset (expected hex, e.g. 0x8000)".to_string())
}

pub fn flash_esp32(
    firmware_path: &std::path::Path,
    port_name: &str,
    config: &Esp32FlashConfig,
) -> Result<(), String> {
    let request = FlashRequest {
        target: FirmwareTarget::Esp32,
        firmware_path: firmware_path.to_path_buf(),
        port_name: Some(port_name.to_string()),
    };

    validate_flash_target(&request, None)?;
    validate_esp32_image(firmware_path)?;
    validate_esp32_flash_config(config)?;
    validate_esp32_firmware_chip(firmware_path, &config.chip)?;
    // Identify the connected chip before flashing.
    let detected_chip = detect_esp32_chip(port_name)?;

    // Block flashing if the connected chip differs
    // from the selected firmware configuration.
    validate_esp32_chip_match(&detected_chip, &config.chip)?;
    // Detect the connected ESP32's physical flash capacity.
    let flash_size = detect_esp32_flash_size(port_name, &config.chip)?;

    // Calculate where the firmware image would end.
    // let firmware_size = std::fs::metadata(firmware_path)
    //     .map_err(|error| {
    //         format!("Cannot read ESP32 firmware size: {error}")
    //     })?
    //     .len();
    // Validate the application image against physical flash capacity.
    let manifest = Esp32FirmwareManifest {
        chip: config.chip.clone(),
        segments: vec![Esp32FirmwareSegment {
            address: config.flash_address,
            path: firmware_path.to_path_buf(),
            component: Esp32FirmwareComponent::Application,
        }],
    };
    validate_esp32_flash_capacity(&manifest, flash_size)?;

    // Require an explicitly configured partition-table location; never guess.
    // Example: PATCHWORK_ESP32_PARTITION_TABLE_OFFSET=0x8000
    let partition_offset = configured_esp32_partition_offset()?;
    if partition_offset < 0x1000 || u64::from(partition_offset) + 0x1000 > flash_size {
        return Err("ESP32 partition-table offset is outside the usable flash region".to_string());
    }
    let partitions = read_esp32_partition_table(port_name, &config.chip, partition_offset)?;
    let firmware_size = std::fs::metadata(firmware_path)
        .map_err(|error| format!("Cannot read ESP32 firmware size: {error}"))?
        .len();
    validate_esp32_factory_flash(
        &partitions,
        config.flash_address,
        firmware_size,
        partition_offset,
        flash_size,
    )?;

    let address_arg = format!("0x{:X}", config.flash_address);
    let output = run_esptool(&[
        std::ffi::OsStr::new("--chip"),
        std::ffi::OsStr::new(&config.chip),
        std::ffi::OsStr::new("--port"),
        std::ffi::OsStr::new(port_name),
        std::ffi::OsStr::new("--baud"),
        std::ffi::OsStr::new("115200"),
        std::ffi::OsStr::new("write-flash"),
        std::ffi::OsStr::new(&address_arg),
        firmware_path.as_os_str(),
    ])?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        return Err(format!(
            "ESP32 flashing failed (exit status: {})\n\nOutput:\n{}\n\nErrors:\n{}",
            output.status,
            stdout.trim(),
            stderr.trim()
        ));
    }

    Ok(())
}
pub fn spawn_esp32_flash_worker(
    firmware_path: std::path::PathBuf,
    port_name: String,
    config: Esp32FlashConfig,
) -> std::sync::mpsc::Receiver<Result<(), String>> {
    let (sender, receiver) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = flash_esp32(&firmware_path, &port_name, &config);

        let _ = sender.send(result);
    });

    receiver
}
#[cfg(target_os = "windows")]
pub fn spawn_pico_flash_worker(
    firmware_path: std::path::PathBuf,
    drive_path: std::path::PathBuf,
) -> std::sync::mpsc::Receiver<Result<(), String>> {
    let (sender, receiver) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let selected_drive = drive_path.canonicalize();

        let discovered_drive = selected_drive
            .as_ref()
            .ok()
            .map(|selected| {
                discover_identified_pico_drives()
                    .iter()
                    .any(|device| device.drive_path.canonicalize().ok().as_ref() == Some(selected))
            })
            .unwrap_or(false);

        let result = if !discovered_drive {
            Err("Selected Pico BOOTSEL drive is no longer available".to_string())
        } else {
            flash_pico_uf2(&firmware_path, &drive_path)
        };

        let _ = sender.send(result);
    });

    receiver
}
#[cfg(target_os = "windows")]
pub fn flash_pico_uf2(
    firmware_path: &std::path::Path,
    drive_path: &std::path::Path,
) -> Result<(), String> {
    // Prevent copying firmware from the destination drive itself.
    let firmware_absolute = std::fs::canonicalize(firmware_path)
        .map_err(|e| format!("Cannot resolve firmware path: {e}"))?;

    let drive_absolute = std::fs::canonicalize(drive_path)
        .map_err(|e| format!("Cannot resolve Pico drive path: {e}"))?;

    if firmware_absolute.starts_with(&drive_absolute) {
        return Err("Firmware source cannot be located on the Pico BOOTSEL drive.".to_string());
    }
    // Confirm that firmware matches the connected Pico.
    let info_path = drive_path.join("INFO_UF2.TXT");

    validate_pico_flash_target(firmware_path, &info_path)?;

    // Copy the UF2 firmware onto the Pico BOOTSEL drive.
    let destination = drive_path.join("firmware.uf2");
    // Never overwrite an existing destination file.
    if destination.exists() {
        return Err("Destination firmware.uf2 already exists. Flash aborted.".to_string());
    }

    use std::io::Write;

    let firmware_data =
        std::fs::read(firmware_path).map_err(|e| format!("Failed to read UF2 firmware: {e}"))?;

    let mut destination_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|e| format!("Failed to create UF2 destination: {e}"))?;

    let write_result = destination_file
        .write_all(&firmware_data)
        .and_then(|_| destination_file.sync_all());

    if let Err(error) = write_result {
        drop(destination_file);

        // Best-effort cleanup of an incomplete firmware file.
        let cleanup_result = std::fs::remove_file(&destination);

        return Err(match cleanup_result {
            Ok(()) => format!("Failed to write or sync UF2 firmware: {error}"),
            Err(cleanup_error) => format!(
                "Failed to write or sync UF2 firmware: {error}. \
             Could not remove incomplete file: {cleanup_error}"
            ),
        });
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    // Exercise the actual flash_esp32() entrypoint with fake device discovery,
    // fake esptool responses, and a simulated 4 KB partition-table download.
    // No subprocess is spawned and no physical flash is accessed.
    #[test]
    fn test_esp32_full_mock_flash_sequence() {
        use md5::{Digest, Md5};
        use std::cell::RefCell;
        use std::ffi::OsStr;
        use std::rc::Rc;

        struct ResetMocks;
        impl Drop for ResetMocks {
            fn drop(&mut self) {
                ESPTOOL_TEST_RUNNER.with(|slot| *slot.borrow_mut() = None);
                ESPTOOL_TEST_DEVICES.with(|slot| *slot.borrow_mut() = None);
                ESPTOOL_TEST_PARTITION_OFFSET.with(|slot| slot.set(None));
            }
        }
        let _reset = ResetMocks;

        fn success(stdout: &str) -> std::process::Output {
            #[cfg(unix)]
            use std::os::unix::process::ExitStatusExt;
            #[cfg(windows)]
            use std::os::windows::process::ExitStatusExt;
            std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            }
        }

        let mut table = vec![0xFFu8; 4096];
        table[0..2].copy_from_slice(&0x50AAu16.to_le_bytes());
        table[2] = 0; // Factory application
        table[3] = 0;
        table[4..8].copy_from_slice(&0x10000u32.to_le_bytes());
        table[8..12].copy_from_slice(&0x100000u32.to_le_bytes());
        table[12..28].fill(0);
        table[12..19].copy_from_slice(b"factory");
        table[32..34].copy_from_slice(&0xEBEBu16.to_le_bytes());
        let digest = Md5::digest(&table[..32]);
        table[48..64].copy_from_slice(&digest);

        let firmware_path = std::env::temp_dir().join(format!(
            "patchwork-mock-firmware-{}-{:?}.bin",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut firmware = vec![0u8; 4096];
        firmware[0] = 0xE9;
        firmware[1] = 1;
        // ESP32 chip ID at bytes 12..14 is zero.
        std::fs::write(&firmware_path, firmware).unwrap();
        struct RemoveFirmware(std::path::PathBuf);
        impl Drop for RemoveFirmware {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = RemoveFirmware(firmware_path.clone());

        let config = Esp32FlashConfig {
            layout: Esp32FirmwareLayout::Application,
            flash_address: 0x10000,
            chip: "esp32".to_string(),
        };
        ESPTOOL_TEST_DEVICES.with(|slot| {
            *slot.borrow_mut() = Some(vec![HardwareDevice {
                port_name: "MOCK_COM7".to_string(),
                vid: 0x303A,
                pid: 1,
                manufacturer: None,
                product: None,
                board_family: "ESP32 simulated device".to_string(),
            }])
        });
        ESPTOOL_TEST_PARTITION_OFFSET.with(|slot| slot.set(Some(0x8000)));

        // Success, chip mismatch, corrupted partition checksum, and
        // simulated write failure: verify that unsafe cases never write.
        for scenario in ["success", "wrong-chip", "bad-md5", "write-error"] {
            let log = Rc::new(RefCell::new(Vec::<String>::new()));
            let log_for_runner = log.clone();
            let mut table_for_scenario = table.clone();
            if scenario == "bad-md5" {
                table_for_scenario[48] ^= 1;
            }
            ESPTOOL_TEST_RUNNER.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |args: &[&OsStr]| {
                    let values: Vec<String> = args
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect();
                    let operation = if values.iter().any(|v| v == "image-info") {
                        "image-info"
                    } else if values.iter().any(|v| v == "chip-id") {
                        "chip-id"
                    } else if values.iter().any(|v| v == "flash-id") {
                        "flash-id"
                    } else if values.iter().any(|v| v == "read-flash") {
                        "read-flash"
                    } else if values.iter().any(|v| v == "write-flash") {
                        "write-flash"
                    } else {
                        panic!("unexpected esptool command: {values:?}")
                    };
                    log_for_runner.borrow_mut().push(operation.to_string());
                    match operation {
                        "image-info" => Ok(success("Mock image is valid")),
                        "chip-id" => Ok(success(if scenario == "wrong-chip" {
                            "Chip is ESP32-S3"
                        } else {
                            "Chip is ESP32"
                        })),
                        "flash-id" => Ok(success("Detected flash size: 4MB")),
                        "read-flash" => {
                            let pos = values.iter().position(|v| v == "read-flash").unwrap();
                            assert_eq!(values[pos + 1], "0x8000");
                            assert_eq!(values[pos + 2], "0x1000");
                            std::fs::write(&values[pos + 3], &table_for_scenario).unwrap();
                            Ok(success("Read complete"))
                        }
                        "write-flash" => {
                            let pos = values.iter().position(|v| v == "write-flash").unwrap();
                            assert_eq!(values[pos + 1], "0x10000");
                            if scenario == "write-error" {
                                Err("mocked write failure; no hardware was accessed".to_string())
                            } else {
                                Ok(success("Simulated write complete"))
                            }
                        }
                        _ => unreachable!(),
                    }
                }));
            });
            let result = flash_esp32(&firmware_path, "MOCK_COM7", &config);
            let calls = log.borrow().clone();
            match scenario {
                "success" => {
                    assert!(result.is_ok(), "mock success failed: {result:?}");
                    assert_eq!(
                        calls,
                        [
                            "image-info",
                            "chip-id",
                            "flash-id",
                            "read-flash",
                            "write-flash"
                        ]
                    );
                }
                "wrong-chip" => {
                    assert!(result.is_err());
                    assert_eq!(calls, ["image-info", "chip-id"]);
                }
                "bad-md5" => {
                    assert!(result.as_ref().unwrap_err().contains("MD5"));
                    assert!(!calls.contains(&"write-flash".to_string()));
                }
                "write-error" => {
                    assert!(result.unwrap_err().contains("mocked write failure"));
                    assert_eq!(calls.last().map(String::as_str), Some("write-flash"));
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn test_esptool_runner_mock_does_not_spawn_process() {
        use std::ffi::OsStr;
        let args = [
            OsStr::new("--port"),
            OsStr::new("COM7"),
            OsStr::new("chip-id"),
        ];
        let result = run_esptool_with(&args, |received| {
            assert_eq!(received, args);
            Err("mocked chip detection failure".to_string())
        });
        assert_eq!(result.unwrap_err(), "mocked chip detection failure");
    }

    #[test]
    fn test_esptool_runner_mock_blocks_write_flash() {
        use std::ffi::OsStr;
        let args = [
            OsStr::new("--chip"),
            OsStr::new("esp32"),
            OsStr::new("--port"),
            OsStr::new("COM7"),
            OsStr::new("write-flash"),
            OsStr::new("0x10000"),
            OsStr::new("firmware.bin"),
        ];
        let result = run_esptool_with(&args, |received| {
            assert!(received.iter().any(|arg| *arg == OsStr::new("write-flash")));
            Err("mock: writing disabled".to_string())
        });
        assert_eq!(result.unwrap_err(), "mock: writing disabled");
    }

    #[test]
    fn rejects_missing_firmware() {
        let request = FlashRequest {
            target: FirmwareTarget::Esp32,
            firmware_path: std::env::temp_dir().join("patchwork_nonexistent_firmware_test.bin"),
            port_name: None,
        };

        assert!(validate_firmware(&request).is_err());
    }

    #[test]
    fn rejects_wrong_extension() {
        let path = std::env::temp_dir().join("patchwork_firmware_extension_test.txt");

        std::fs::write(&path, b"test").unwrap();

        let request = FlashRequest {
            target: FirmwareTarget::Esp32,
            firmware_path: path.clone(),
            port_name: None,
        };

        let result = validate_firmware(&request);

        std::fs::remove_file(&path).unwrap();

        assert!(result.is_err());
    }

    #[test]
    fn rejects_empty_firmware() {
        let path = std::env::temp_dir().join("patchwork_firmware_empty_test.bin");

        std::fs::write(&path, b"").unwrap();

        let request = FlashRequest {
            target: FirmwareTarget::Esp32,
            firmware_path: path.clone(),
            port_name: None,
        };

        let result = validate_firmware(&request);

        std::fs::remove_file(&path).unwrap();

        assert!(result.is_err());
    }
    #[test]
    fn test_esp32_rejects_invalid_image() {
        let firmware_path = std::path::Path::new("test_firmware.bin");

        let result = super::validate_esp32_image(firmware_path);

        assert!(result.is_err(), "Invalid ESP32 firmware should be rejected");
    }

    #[test]
    fn rejects_invalid_uf2_magic() {
        let path = std::env::temp_dir().join("patchwork_invalid_magic_test.uf2");

        std::fs::write(&path, vec![0u8; 512]).unwrap();

        let result = validate_uf2_header(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(result.is_err());
    }

    #[test]
    fn rejects_incomplete_uf2_block() {
        let path = std::env::temp_dir().join("patchwork_incomplete_block_test.uf2");

        let mut data = vec![0u8; 100];

        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        std::fs::write(&path, data).unwrap();

        let result = validate_uf2_header(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(result.is_err());
    }
    #[test]
    fn accepts_valid_uf2_header() {
        let path = std::env::temp_dir().join("patchwork_valid_header_test.uf2");

        let mut data = vec![0u8; 512];

        // UF2 start magic numbers
        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());
        // UF2 payload size: 256 bytes
        data[16..20].copy_from_slice(&256u32.to_le_bytes());
        // This synthetic firmware contains exactly one block.
        data[24..28].copy_from_slice(&1u32.to_le_bytes());
        // UF2 ending magic number
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&path, data).unwrap();

        let result = validate_uf2_header(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(
            result.is_ok(),
            "Expected valid UF2 header, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_invalid_uf2_end_magic() {
        let path = std::env::temp_dir().join("patchwork_invalid_end_magic_test.uf2");

        let mut data = vec![0u8; 512];

        // Valid UF2 start magic numbers
        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        // Deliberately leave the ending magic number invalid.
        std::fs::write(&path, data).unwrap();

        let result = validate_uf2_header(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(
            result.is_err(),
            "Expected invalid UF2 ending magic to be rejected"
        );
    }

    #[test]
    fn rejects_invalid_second_block_magic() {
        let path = std::env::temp_dir().join("patchwork_invalid_second_block_test.uf2");

        // Create two UF2 blocks (512 bytes each).
        let mut data = vec![0u8; 1024];

        for i in 0..2 {
            let offset = i * 512;

            // Valid starting magic numbers.
            data[offset..offset + 4].copy_from_slice(&0x0A324655u32.to_le_bytes());

            data[offset + 4..offset + 8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

            // Payload size: 256 bytes.
            data[offset + 16..offset + 20].copy_from_slice(&256u32.to_le_bytes());

            // Block number.
            data[offset + 20..offset + 24].copy_from_slice(&(i as u32).to_le_bytes());

            // Total blocks: 2.
            data[offset + 24..offset + 28].copy_from_slice(&2u32.to_le_bytes());

            // Valid ending magic.
            data[offset + 508..offset + 512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());
        }

        // Corrupt the second block's first magic number.
        data[512..516].copy_from_slice(&0u32.to_le_bytes());

        std::fs::write(&path, data).unwrap();

        let result = validate_uf2_header(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("Invalid UF2 starting magic in block 1") }),
            "Expected second-block magic validation error, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_corrupted_pico_firmware() {
        let path = std::env::temp_dir().join("patchwork_corrupted_pico_test.uf2");

        // Create a UF2-sized file containing invalid data.
        std::fs::write(&path, vec![0u8; 512]).unwrap();

        let request = FlashRequest {
            target: FirmwareTarget::RaspberryPiPico,
            firmware_path: path.clone(),
            port_name: None,
        };

        let result = validate_firmware(&request);

        std::fs::remove_file(&path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("Invalid UF2 magic numbers") }),
            "Expected corrupted Pico firmware to be rejected, got: {:?}",
            result
        );
    }

    #[test]
    fn reads_rp2040_uf2_family_id() {
        let path = std::env::temp_dir().join("patchwork_rp2040_family_test.uf2");

        let mut data = vec![0u8; 512];

        // UF2 starting magic numbers.
        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        // Family ID present flag.
        data[8..12].copy_from_slice(&0x00002000u32.to_le_bytes());

        // Payload size: 256 bytes.
        data[16..20].copy_from_slice(&256u32.to_le_bytes());

        // Total blocks: 1 (block number defaults to 0).
        data[24..28].copy_from_slice(&1u32.to_le_bytes());

        // RP2040 family ID.
        data[28..32].copy_from_slice(&0xE48BFF56u32.to_le_bytes());

        // UF2 ending magic number.
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&path, data).unwrap();

        let result = read_uf2_family_id(&path);

        std::fs::remove_file(&path).unwrap();

        assert_eq!(
            result.unwrap(),
            Some(0xE48BFF56),
            "Expected RP2040 family ID"
        );
    }
    #[test]
    fn reads_rp2350_uf2_family_id() {
        let path = std::env::temp_dir().join("patchwork_rp2350_family_test.uf2");

        let mut data = vec![0u8; 512];

        // UF2 starting magic numbers.
        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        // Family ID present flag.
        data[8..12].copy_from_slice(&0x00002000u32.to_le_bytes());

        // Payload size: 256 bytes.
        data[16..20].copy_from_slice(&256u32.to_le_bytes());

        // Total blocks: 1.
        data[24..28].copy_from_slice(&1u32.to_le_bytes());

        // RP2350 ARM Secure family ID.
        data[28..32].copy_from_slice(&0xE48BFF59u32.to_le_bytes());

        // UF2 ending magic number.
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&path, data).unwrap();

        let result = read_uf2_family_id(&path);

        std::fs::remove_file(&path).unwrap();

        assert_eq!(
            result.unwrap(),
            Some(0xE48BFF59),
            "Expected RP2350 ARM Secure family ID"
        );
    }
    #[test]
    fn rejects_rp2040_firmware_for_rp2350() {
        let path = std::env::temp_dir().join("patchwork_wrong_pico_family_test.uf2");

        let mut data = vec![0u8; 512];

        // Valid UF2 starting magic numbers.
        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        // Family ID present.
        data[8..12].copy_from_slice(&0x00002000u32.to_le_bytes());

        // Payload size: 256 bytes.
        data[16..20].copy_from_slice(&256u32.to_le_bytes());

        // Total blocks: 1.
        data[24..28].copy_from_slice(&1u32.to_le_bytes());

        // RP2040 firmware family ID.
        data[28..32].copy_from_slice(&0xE48BFF56u32.to_le_bytes());

        // Valid UF2 ending magic.
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&path, data).unwrap();

        // Attempt to validate RP2040 firmware for an RP2350 board.
        let result = validate_pico_compatibility(&path, PicoBoard::Rp2350);

        std::fs::remove_file(&path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("not compatible") }),
            "Expected incompatible firmware to be rejected, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_rp2350_firmware_for_rp2040() {
        let path = std::env::temp_dir().join("patchwork_rp2350_wrong_board_test.uf2");

        let mut data = vec![0u8; 512];

        // Valid UF2 starting magic numbers.
        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        // Family ID present.
        data[8..12].copy_from_slice(&0x00002000u32.to_le_bytes());

        // Payload size: 256 bytes.
        data[16..20].copy_from_slice(&256u32.to_le_bytes());

        // Total blocks: 1.
        data[24..28].copy_from_slice(&1u32.to_le_bytes());

        // RP2350 ARM Secure firmware family ID.
        data[28..32].copy_from_slice(&0xE48BFF59u32.to_le_bytes());

        // Valid UF2 ending magic.
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&path, data).unwrap();

        // Attempt to use RP2350 firmware on an RP2040 board.
        let result = validate_pico_compatibility(&path, PicoBoard::Rp2040);

        std::fs::remove_file(&path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("not compatible") }),
            "Expected incompatible firmware to be rejected, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_uf2_without_family_id() {
        let path = std::env::temp_dir().join("patchwork_missing_family_test.uf2");

        let mut data = vec![0u8; 512];

        // Valid UF2 starting magic numbers.
        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        // Flags remain zero: no family ID declared.

        // Payload size: 256 bytes.
        data[16..20].copy_from_slice(&256u32.to_le_bytes());

        // Total blocks: 1.
        data[24..28].copy_from_slice(&1u32.to_le_bytes());

        // Valid UF2 ending magic.
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&path, data).unwrap();

        let result = validate_pico_compatibility(&path, PicoBoard::Rp2040);

        std::fs::remove_file(&path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("does not declare a family ID") }),
            "Expected missing family ID to be rejected, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_inconsistent_uf2_family_ids() {
        let path = std::env::temp_dir().join("patchwork_inconsistent_family_test.uf2");

        // Two UF2 blocks, 512 bytes each.
        let mut data = vec![0u8; 1024];

        for i in 0..2usize {
            let offset = i * 512;

            // UF2 starting magic numbers.
            data[offset..offset + 4].copy_from_slice(&0x0A324655u32.to_le_bytes());

            data[offset + 4..offset + 8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

            // Family ID present flag.
            data[offset + 8..offset + 12].copy_from_slice(&0x00002000u32.to_le_bytes());

            // Payload size: 256 bytes.
            data[offset + 16..offset + 20].copy_from_slice(&256u32.to_le_bytes());

            // Block number.
            data[offset + 20..offset + 24].copy_from_slice(&(i as u32).to_le_bytes());

            // Total blocks: 2.
            data[offset + 24..offset + 28].copy_from_slice(&2u32.to_le_bytes());

            // Different family IDs.
            let family_id = if i == 0 {
                0xE48BFF56u32 // RP2040
            } else {
                0xE48BFF59u32 // RP2350
            };

            data[offset + 28..offset + 32].copy_from_slice(&family_id.to_le_bytes());

            // UF2 ending magic.
            data[offset + 508..offset + 512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());
        }

        std::fs::write(&path, data).unwrap();

        let result = read_uf2_family_id(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("Inconsistent UF2 family ID in block 1") }),
            "Expected inconsistent family IDs to be rejected, got: {:?}",
            result
        );
    }
    #[test]
    fn identifies_rp2040_bootloader() {
        let path = std::env::temp_dir().join("patchwork_rp2040_info_test.txt");

        let content = "\
UF2 Bootloader v3.0
Model: Raspberry Pi RP2
Board-ID: RPI-RP2
Chip: RP2040
";

        std::fs::write(&path, content).unwrap();

        let result = identify_pico_bootloader(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(
            matches!(result, Ok(PicoBoard::Rp2040)),
            "Expected RP2040 bootloader, got: {:?}",
            result
        );
    }
    #[test]
    fn identifies_rp2350_bootloader() {
        let path = std::env::temp_dir().join("patchwork_rp2350_info_test.txt");

        let content = "\
UF2 Bootloader
Model: Raspberry Pi RP2350
Board-ID: RP2350
Chip: RP2350
";

        std::fs::write(&path, content).unwrap();

        let result = identify_pico_bootloader(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(
            matches!(result, Ok(PicoBoard::Rp2350)),
            "Expected RP2350 bootloader, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_unknown_pico_bootloader() {
        let path = std::env::temp_dir().join("patchwork_unknown_bootloader_test.txt");

        let content = "\
UF2 Bootloader
Model: Unknown USB Device
Board-ID: UNKNOWN-BOARD
";

        std::fs::write(&path, content).unwrap();

        let result = identify_pico_bootloader(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("Unknown Pico bootloader") }),
            "Expected unknown bootloader to be rejected, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_wrong_firmware_for_detected_pico() {
        let dir = std::env::temp_dir();
        let firmware_path = dir.join("patchwork_combined_test.uf2");
        let info_path = dir.join("patchwork_combined_info_test.txt");

        // Create a structurally valid RP2040 UF2 test block.
        let mut data = vec![0u8; 512];

        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());
        data[8..12].copy_from_slice(&0x00002000u32.to_le_bytes());
        data[16..20].copy_from_slice(&256u32.to_le_bytes());
        data[24..28].copy_from_slice(&1u32.to_le_bytes());
        data[28..32].copy_from_slice(&0xE48BFF56u32.to_le_bytes());
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&firmware_path, data).unwrap();

        // Simulate an RP2350 bootloader.
        std::fs::write(
            &info_path,
            "UF2 Bootloader\nModel: Raspberry Pi RP2350\nChip: RP2350\n",
        )
        .unwrap();

        let result = validate_pico_flash_target(&firmware_path, &info_path);

        std::fs::remove_file(&firmware_path).unwrap();
        std::fs::remove_file(&info_path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("not compatible") }),
            "Expected incompatible firmware to be rejected, got: {:?}",
            result
        );
    }
    #[test]
    fn accepts_matching_firmware_for_detected_pico() {
        let dir = std::env::temp_dir();
        let firmware_path = dir.join("patchwork_matching_pico_test.uf2");
        let info_path = dir.join("patchwork_matching_pico_info.txt");

        // Create a structurally valid RP2040 UF2 test block.
        let mut data = vec![0u8; 512];

        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        // Family ID present.
        data[8..12].copy_from_slice(&0x00002000u32.to_le_bytes());

        // Payload size: 256 bytes.
        data[16..20].copy_from_slice(&256u32.to_le_bytes());

        // Total blocks: 1.
        data[24..28].copy_from_slice(&1u32.to_le_bytes());

        // RP2040 family ID.
        data[28..32].copy_from_slice(&0xE48BFF56u32.to_le_bytes());

        // UF2 ending magic.
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&firmware_path, data).unwrap();

        // Simulate a matching RP2040 bootloader.
        std::fs::write(
            &info_path,
            "UF2 Bootloader\nModel: Raspberry Pi RP2\nChip: RP2040\n",
        )
        .unwrap();

        let result = validate_pico_flash_target(&firmware_path, &info_path);

        std::fs::remove_file(&firmware_path).unwrap();
        std::fs::remove_file(&info_path).unwrap();

        assert!(
            matches!(result, Ok(PicoBoard::Rp2040)),
            "Expected matching RP2040 firmware to be accepted, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_esp32_flash_without_selected_port() {
        let firmware_path = std::env::temp_dir().join("patchwork_no_port_test.bin");

        // Create a nonempty test file.
        std::fs::write(&firmware_path, b"TEST").unwrap();

        let request = FlashRequest {
            target: FirmwareTarget::Esp32,
            firmware_path: firmware_path.clone(),
            port_name: None,
        };

        let result = validate_flash_target(&request, None);

        std::fs::remove_file(&firmware_path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("No ESP32 serial port selected") }),
            "Expected missing serial port to be rejected, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_pico_flash_without_selected_drive() {
        let firmware_path = std::env::temp_dir().join("patchwork_no_pico_drive_test.uf2");

        // Create a structurally valid RP2040 UF2 block.
        let mut data = vec![0u8; 512];

        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        // Family ID present.
        data[8..12].copy_from_slice(&0x00002000u32.to_le_bytes());

        // Payload size: 256 bytes.
        data[16..20].copy_from_slice(&256u32.to_le_bytes());

        // Total blocks: 1.
        data[24..28].copy_from_slice(&1u32.to_le_bytes());

        // RP2040 family ID.
        data[28..32].copy_from_slice(&0xE48BFF56u32.to_le_bytes());

        // UF2 ending magic.
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&firmware_path, data).unwrap();

        let request = FlashRequest {
            target: FirmwareTarget::RaspberryPiPico,
            firmware_path: firmware_path.clone(),
            port_name: None,
        };

        // No Pico BOOTSEL drive selected.
        let result = validate_flash_target(&request, None);

        std::fs::remove_file(&firmware_path).unwrap();

        assert!(
            result
                .as_ref()
                .is_err_and(|error| { error.contains("No Pico BOOTSEL drive selected") }),
            "Expected missing Pico drive to be rejected, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_esp32_firmware_with_pico_drive() {
        let firmware_path = std::env::temp_dir().join("patchwork_esp32_pico_mismatch_test.bin");

        std::fs::write(&firmware_path, b"TEST").unwrap();

        let request = FlashRequest {
            target: FirmwareTarget::Esp32,
            firmware_path: firmware_path.clone(),
            port_name: Some("COM3".to_string()),
        };

        // Simulate a Pico BOOTSEL drive being selected.
        let pico_drive = std::path::Path::new("E:\\");

        let result = validate_flash_target(&request, Some(pico_drive));

        std::fs::remove_file(&firmware_path).unwrap();

        assert!(
            result.as_ref().is_err_and(|error| {
                error.contains("ESP32 firmware cannot target a Pico BOOTSEL drive")
            }),
            "Expected ESP32/Pico mismatch to be rejected, got: {:?}",
            result
        );
    }
    #[test]
    fn rejects_pico_firmware_with_serial_port() {
        let firmware_path = std::env::temp_dir().join("patchwork_pico_serial_mismatch_test.uf2");

        // Create a structurally valid RP2040 UF2 block.
        let mut data = vec![0u8; 512];

        data[0..4].copy_from_slice(&0x0A324655u32.to_le_bytes());
        data[4..8].copy_from_slice(&0x9E5D5157u32.to_le_bytes());

        // Family ID present.
        data[8..12].copy_from_slice(&0x00002000u32.to_le_bytes());

        // Payload size: 256 bytes.
        data[16..20].copy_from_slice(&256u32.to_le_bytes());

        // Total blocks: 1.
        data[24..28].copy_from_slice(&1u32.to_le_bytes());

        // RP2040 family ID.
        data[28..32].copy_from_slice(&0xE48BFF56u32.to_le_bytes());

        // UF2 ending magic.
        data[508..512].copy_from_slice(&0x0AB16F30u32.to_le_bytes());

        std::fs::write(&firmware_path, data).unwrap();

        let request = FlashRequest {
            target: FirmwareTarget::RaspberryPiPico,
            firmware_path: firmware_path.clone(),
            port_name: Some("COM3".to_string()),
        };

        let result = validate_flash_target(&request, None);

        std::fs::remove_file(&firmware_path).unwrap();

        assert!(
            result.as_ref().is_err_and(|error| {
                error.contains("Pico UF2 firmware requires a BOOTSEL drive")
            }),
            "Expected Pico/serial mismatch to be rejected, got: {:?}",
            result
        );
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn pico_flash_rejects_missing_firmware() {
        let firmware = std::env::temp_dir().join("patchwork_missing_firmware_test.uf2");

        let drive = std::env::temp_dir();

        // Ensure the test firmware does not exist.
        let _ = std::fs::remove_file(&firmware);

        let result = super::flash_pico_uf2(&firmware, &drive);

        assert!(result.is_err());
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn pico_flash_copies_to_simulated_drive() {
        let temp = std::env::temp_dir();

        let firmware = temp.join("patchwork_test_rp2040.uf2");

        // Create a separate simulated Pico drive for this test.
        let drive = temp.join(format!("patchwork_pico_copy_test_{}", std::process::id()));

        assert!(firmware.is_file(), "Test UF2 file is missing");

        std::fs::create_dir_all(&drive).unwrap();

        std::fs::write(
            drive.join("INFO_UF2.TXT"),
            "UF2 Bootloader v3.0\nModel: Raspberry Pi RP2040\nBoard-ID: RPI-RP2\n",
        )
        .unwrap();

        let destination = drive.join("firmware.uf2");

        assert!(!destination.exists(), "Test destination already exists");

        // Copy firmware to simulated Pico drive.
        let result = super::flash_pico_uf2(&firmware, &drive);

        assert!(result.is_ok(), "Flash simulation failed: {result:?}");

        // Verify copied firmware matches original.
        let source_bytes = std::fs::read(&firmware).unwrap();
        let copied_bytes = std::fs::read(&destination).unwrap();

        assert_eq!(source_bytes, copied_bytes);

        // Clean up the simulated Pico drive.
        std::fs::remove_dir_all(&drive).expect("Failed to clean up simulated Pico drive");
    }
    #[test]
    fn identifies_rp2040_from_rpi_rp2_board_id() {
        let info_path =
            std::env::temp_dir().join(format!("patchwork_rpi_rp2_{}.txt", std::process::id()));

        std::fs::write(
            &info_path,
            "UF2 Bootloader v3.0\nModel: Raspberry Pi RP2\nBoard-ID: RPI-RP2\n",
        )
        .unwrap();

        let result = super::identify_pico_bootloader(&info_path);

        std::fs::remove_file(&info_path).unwrap();

        assert!(matches!(result, Ok(super::PicoBoard::Rp2040)));
    }
    #[test]
    fn rejects_conflicting_pico_bootloader_identifiers() {
        let info_path = std::env::temp_dir().join(format!(
            "patchwork_conflicting_pico_{}.txt",
            std::process::id()
        ));

        std::fs::write(
            &info_path,
            "UF2 Bootloader v3.0\nModel: RP2350\nBoard-ID: RPI-RP2\n",
        )
        .unwrap();

        let result = super::identify_pico_bootloader(&info_path);

        std::fs::remove_file(&info_path).unwrap();

        assert!(
            matches!(&result, Err(message) if message.contains("Conflicting")),
            "Expected conflicting board identifiers to be rejected, got: {result:?}"
        );
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn pico_flash_rejects_existing_destination() {
        let temp = std::env::temp_dir();

        let firmware = temp.join("patchwork_test_rp2040.uf2");

        // Use a separate simulated Pico drive.
        let drive = temp.join(format!(
            "patchwork_pico_overwrite_test_{}",
            std::process::id()
        ));

        assert!(firmware.is_file(), "Test UF2 file is missing");

        std::fs::create_dir_all(&drive).unwrap();

        std::fs::write(
            drive.join("INFO_UF2.TXT"),
            "UF2 Bootloader v3.0\nModel: Raspberry Pi RP2040\nBoard-ID: RPI-RP2\n",
        )
        .unwrap();

        let destination = drive.join("firmware.uf2");

        assert!(!destination.exists(), "Test destination already exists");

        // Create a file that must not be overwritten.
        let original_data = b"DO NOT OVERWRITE";

        std::fs::write(&destination, original_data).unwrap();

        // Attempt to flash firmware.
        let result = super::flash_pico_uf2(&firmware, &drive);

        // Verify that the existing file remains unchanged.
        let saved_data = std::fs::read(&destination).unwrap();

        assert!(result.is_err(), "Expected flashing to be rejected");
        assert_eq!(saved_data, original_data);

        // Clean up the simulated drive.
        std::fs::remove_dir_all(&drive).expect("Failed to clean up simulated Pico drive");
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn rejects_non_removable_pico_locations() {
        use std::path::{Path, PathBuf};

        // A normal folder must never be treated as a removable drive.
        let folder = std::env::temp_dir().join("patchwork_pico_simulation");

        assert!(!super::is_windows_removable_drive(&folder));

        // The system drive is a fixed local disk on this test machine.
        let system_drive = Path::new("C:\\");

        assert!(!super::is_windows_removable_drive(system_drive));
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn pico_discovery_only_returns_removable_drive_roots() {
        let devices = super::discover_identified_pico_drives();

        for device in devices {
            assert!(
                super::is_windows_drive_root(&device.drive_path),
                "Pico discovery returned a non-root path: {:?}",
                device.drive_path
            );

            assert!(
                super::is_windows_removable_drive(&device.drive_path),
                "Pico discovery returned a non-removable drive: {:?}",
                device.drive_path
            );
        }
    }
    #[test]
    fn test_esp32_rejects_incomplete_image() {
        let firmware_path = std::env::temp_dir().join(format!(
            "patchwork_esp32_invalid_{}.bin",
            std::process::id()
        ));

        // Valid-looking header, but incomplete ESP32 image.
        let invalid_image = [0xE9, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

        std::fs::write(&firmware_path, invalid_image).expect("Failed to create test firmware");

        let result = super::validate_esp32_image(&firmware_path);

        assert!(
            matches!(&result, Err(message) if message.contains(
                "ESP32 firmware image validation failed"
            )),
            "Expected esptool validation failure, got: {:?}",
            result
        );

        let _ = std::fs::remove_file(&firmware_path);

        assert!(result.is_err(), "Incomplete ESP32 image should be rejected");
    }
    #[test]
    fn test_esp32_flash_address_validation() {
        use super::validate_esp32_flash_address;

        // Reject address 0x0 in the current implementation.
        assert!(validate_esp32_flash_address(0x0).is_err());

        // Reject addresses that are not 4 KB aligned.
        assert!(validate_esp32_flash_address(0x1001).is_err());
        assert!(validate_esp32_flash_address(0x1234).is_err());

        // Accept aligned, nonzero addresses.
        assert!(validate_esp32_flash_address(0x1000).is_err());
        assert!(validate_esp32_flash_address(0x8000).is_err());
        assert!(validate_esp32_flash_address(0x10000).is_ok());
        assert!(validate_esp32_flash_address(0x20000).is_ok());
    }

    #[test]
    fn test_esp32_flash_config_validation() {
        use super::{Esp32FirmwareLayout, Esp32FlashConfig, validate_esp32_flash_config};

        // Valid application configuration
        let app_config = Esp32FlashConfig {
            layout: Esp32FirmwareLayout::Application,
            flash_address: 0x10000,
            chip: "esp32".to_string(),
        };

        assert!(validate_esp32_flash_config(&app_config).is_ok());

        // Valid merged-image configuration
        let merged_config = Esp32FlashConfig {
            layout: Esp32FirmwareLayout::Merged,
            flash_address: 0x0,
            chip: "esp32".to_string(),
        };

        assert!(
            validate_esp32_flash_config(&merged_config).is_err(),
            "Merged ESP32 firmware should be rejected until supported"
        );

        // Reject merged image with incorrect starting address
        let invalid_merged = Esp32FlashConfig {
            layout: Esp32FirmwareLayout::Merged,
            flash_address: 0x10000,
            chip: "esp32".to_string(),
        };

        assert!(validate_esp32_flash_config(&invalid_merged).is_err());

        // Reject missing chip type
        let invalid_chip = Esp32FlashConfig {
            layout: Esp32FirmwareLayout::Application,
            flash_address: 0x10000,
            chip: "".to_string(),
        };

        assert!(validate_esp32_flash_config(&invalid_chip).is_err());
    }
    #[test]
    fn test_esp32_supported_chip_validation() {
        use super::{Esp32FirmwareLayout, Esp32FlashConfig, validate_esp32_flash_config};

        let mut config = Esp32FlashConfig {
            layout: Esp32FirmwareLayout::Application,
            flash_address: 0x10000,
            chip: "esp32".to_string(),
        };

        // Standard ESP32
        assert!(validate_esp32_flash_config(&config).is_ok());

        // ESP32-S3
        config.chip = "esp32s3".to_string();
        assert!(validate_esp32_flash_config(&config).is_ok());

        // ESP32-C3
        config.chip = "esp32c3".to_string();
        assert!(validate_esp32_flash_config(&config).is_ok());

        // Unsupported chip
        config.chip = "invalid_chip".to_string();
        assert!(validate_esp32_flash_config(&config).is_err());

        // Automatic selection is not allowed
        config.chip = "auto".to_string();
        assert!(validate_esp32_flash_config(&config).is_err());
    }
    #[test]
    fn test_esp32_manifest_rejects_overlapping_segments() {
        use super::{Esp32FirmwareManifest, Esp32FirmwareSegment, validate_esp32_manifest};

        let temp_dir =
            std::env::temp_dir().join(format!("patchwork_esp32_overlap_{}", std::process::id()));

        std::fs::create_dir_all(&temp_dir).unwrap();

        let bootloader = temp_dir.join("bootloader.bin");
        let partitions = temp_dir.join("partitions.bin");

        // Bootloader occupies 0x1000 through 0x8FFF.
        std::fs::write(&bootloader, vec![0xFF; 0x8000]).unwrap();

        // Partition table begins at 0x8000, causing overlap.
        std::fs::write(&partitions, vec![0xFF; 0x1000]).unwrap();

        let manifest = Esp32FirmwareManifest {
            chip: "esp32".to_string(),
            segments: vec![
                Esp32FirmwareSegment {
                    address: 0x1000,
                    path: bootloader,
                    component: super::Esp32FirmwareComponent::Bootloader,
                },
                Esp32FirmwareSegment {
                    address: 0x8000,
                    path: partitions,
                    component: super::Esp32FirmwareComponent::PartitionTable,
                },
            ],
        };

        let result = validate_esp32_manifest(&manifest);

        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.contains("Overlapping")),
            "Expected overlapping firmware regions to be rejected, got: {:?}",
            result
        );

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }
    #[test]
    fn test_esp32_manifest_chip_validation() {
        use super::{Esp32FirmwareManifest, Esp32FirmwareSegment, validate_esp32_manifest};

        let firmware_path = std::env::temp_dir().join(format!(
            "patchwork_manifest_chip_{}.bin",
            std::process::id()
        ));

        std::fs::write(&firmware_path, vec![0xFF; 4096]).unwrap();

        let make_manifest = |chip: &str| Esp32FirmwareManifest {
            chip: chip.to_string(),
            segments: vec![Esp32FirmwareSegment {
                address: 0x10000,
                path: firmware_path.clone(),
                component: super::Esp32FirmwareComponent::Application,
            }],
        };

        // Supported chip
        assert!(validate_esp32_manifest(&make_manifest("esp32")).is_ok());

        // Another supported chip
        assert!(validate_esp32_manifest(&make_manifest("esp32s3")).is_ok());

        // Unsupported chip
        assert!(validate_esp32_manifest(&make_manifest("invalid_chip")).is_err());

        // Automatic chip selection is not allowed
        assert!(validate_esp32_manifest(&make_manifest("auto")).is_err());

        // Empty chip name
        assert!(validate_esp32_manifest(&make_manifest("")).is_err());

        std::fs::remove_file(&firmware_path).unwrap();
    }
    #[test]
    fn test_esp32_flash_capacity_validation() {
        use super::{Esp32FirmwareManifest, Esp32FirmwareSegment, validate_esp32_flash_capacity};

        let firmware_path = std::env::temp_dir().join(format!(
            "patchwork_flash_capacity_{}.bin",
            std::process::id()
        ));

        // Create a temporary 1 MB dummy firmware file.
        std::fs::write(&firmware_path, vec![0xFF; 1024 * 1024]).unwrap();

        let manifest = Esp32FirmwareManifest {
            chip: "esp32".to_string(),
            segments: vec![Esp32FirmwareSegment {
                address: 0x10000,
                path: firmware_path.clone(),
                component: super::Esp32FirmwareComponent::Application,
            }],
        };

        // 4 MB flash: firmware fits.
        assert!(validate_esp32_flash_capacity(&manifest, 4 * 1024 * 1024).is_ok());

        // 1 MB flash: firmware exceeds capacity.
        assert!(validate_esp32_flash_capacity(&manifest, 1024 * 1024).is_err());

        // Unknown flash capacity must be rejected.
        assert!(validate_esp32_flash_capacity(&manifest, 0).is_err());

        std::fs::remove_file(&firmware_path).unwrap();
    }
    #[test]
    fn test_esp32_firmware_package_validation() {
        use super::{Esp32FirmwareManifest, Esp32FirmwareSegment, validate_esp32_firmware_package};

        let firmware_path = std::env::temp_dir().join(format!(
            "patchwork_package_validation_{}.bin",
            std::process::id()
        ));

        // Create a temporary 4 KB dummy file.
        std::fs::write(&firmware_path, vec![0xFF; 4096]).unwrap();

        let mut manifest = Esp32FirmwareManifest {
            chip: "esp32".to_string(),
            segments: vec![Esp32FirmwareSegment {
                address: 0x10000,
                path: firmware_path.clone(),
                component: super::Esp32FirmwareComponent::Application,
            }],
        };

        // Valid manifest and sufficient flash capacity.
        // Manifest and capacity are valid, but firmware content is invalid.
        let result = validate_esp32_firmware_package(&manifest, 4 * 1024 * 1024);

        assert!(
            result.is_err(),
            "Invalid ESP32 firmware content must be rejected"
        );
        // Invalid chip must be rejected.
        manifest.chip = "invalid_chip".to_string();

        assert!(validate_esp32_firmware_package(&manifest, 4 * 1024 * 1024).is_err());

        // Restore supported chip.
        manifest.chip = "esp32".to_string();

        // Insufficient flash capacity must be rejected.
        assert!(validate_esp32_firmware_package(&manifest, 0x10000).is_err());

        // Unknown flash capacity must be rejected.
        assert!(validate_esp32_firmware_package(&manifest, 0).is_err());

        std::fs::remove_file(&firmware_path).unwrap();
    }
    #[test]
    fn test_esp32_segment_content_validation() {
        use super::{Esp32FirmwareComponent, Esp32FirmwareSegment, validate_esp32_segment_content};

        let firmware_path = std::env::temp_dir().join(format!(
            "patchwork_segment_content_{}.bin",
            std::process::id()
        ));

        // Deliberately invalid ESP32 firmware.
        std::fs::write(&firmware_path, [1, 2, 3, 4]).unwrap();

        let mut segment = Esp32FirmwareSegment {
            address: 0x10000,
            path: firmware_path.clone(),
            component: Esp32FirmwareComponent::Application,
        };

        // Invalid application image must be rejected.
        assert!(validate_esp32_segment_content(&segment).is_err());

        // Invalid bootloader image must be rejected.
        segment.component = Esp32FirmwareComponent::Bootloader;

        assert!(validate_esp32_segment_content(&segment).is_err());

        // Partition-table validation is not implemented yet.
        segment.component = Esp32FirmwareComponent::PartitionTable;

        let result = validate_esp32_segment_content(&segment);
        assert!(
            result
                .as_ref()
                .is_err_and(|e| e.contains("not yet implemented"))
        );

        // Data-component validation is not implemented yet.
        segment.component = Esp32FirmwareComponent::Data;

        let result = validate_esp32_segment_content(&segment);
        assert!(
            result
                .as_ref()
                .is_err_and(|e| e.contains("not yet implemented"))
        );

        std::fs::remove_file(&firmware_path).unwrap();
    }
    #[test]
    fn test_esp32_partition_overlap_validation() {
        use super::{Esp32Partition, validate_esp32_partition_overlaps};

        let factory = Esp32Partition {
            partition_type: 0x00,
            subtype: 0x00,
            offset: 0x10000,
            size: 0x100000,
            label: "factory".to_string(),
        };

        // Valid: begins exactly where the factory partition ends.
        let valid = Esp32Partition {
            partition_type: 0x01,
            subtype: 0x82,
            offset: 0x110000,
            size: 0x10000,
            label: "storage".to_string(),
        };

        assert!(validate_esp32_partition_overlaps(&[factory.clone(), valid,]).is_ok());

        // Invalid: overlaps the factory partition.
        let overlapping = Esp32Partition {
            partition_type: 0x01,
            subtype: 0x82,
            offset: 0x100000,
            size: 0x20000,
            label: "overlapping_storage".to_string(),
        };

        assert!(validate_esp32_partition_overlaps(&[factory, overlapping,]).is_err());
    }
    #[test]
    fn test_esp32_factory_flash_safety() {
        use super::{Esp32Partition, validate_esp32_factory_flash};
        let factory = Esp32Partition {
            partition_type: 0,
            subtype: 0,
            offset: 0x10000,
            size: 0x100000,
            label: "factory".to_string(),
        };
        let flash_size = 4 * 1024 * 1024;
        assert!(
            validate_esp32_factory_flash(&[factory.clone()], 0x10000, 0x80000, 0x8000, flash_size)
                .is_ok()
        );
        assert!(
            validate_esp32_factory_flash(&[factory.clone()], 0x11000, 0x80000, 0x8000, flash_size)
                .is_err()
        );
        assert!(
            validate_esp32_factory_flash(&[factory.clone()], 0x10000, 0x110000, 0x8000, flash_size)
                .is_err()
        );
        assert!(
            validate_esp32_factory_flash(&[factory.clone()], 0x10000, 0x80000, 0x10000, flash_size)
                .is_err()
        );
        let ota = Esp32Partition {
            subtype: 0x10,
            label: "ota_0".to_string(),
            ..factory.clone()
        };
        assert!(
            validate_esp32_factory_flash(
                &[factory.clone(), ota],
                0x10000,
                0x80000,
                0x8000,
                flash_size
            )
            .is_err()
        );
        assert!(validate_esp32_factory_flash(&[], 0x10000, 0x80000, 0x8000, flash_size).is_err());
        let oversized = Esp32Partition {
            size: 0x500000,
            ..factory
        };
        assert!(
            validate_esp32_factory_flash(&[oversized], 0x10000, 0x80000, 0x8000, flash_size)
                .is_err()
        );
    }

    #[test]
    fn test_esp32_partition_table_md5() {
        use super::parse_esp32_partition_table;
        use md5::{Digest, Md5};

        let mut data = vec![0xFF_u8; 128];
        data[0..2].copy_from_slice(&0x50AA_u16.to_le_bytes());
        data[2] = 0x00;
        data[3] = 0x00;
        data[4..8].copy_from_slice(&0x10000_u32.to_le_bytes());
        data[8..12].copy_from_slice(&0x100000_u32.to_le_bytes());
        data[12..28].fill(0);
        data[12..19].copy_from_slice(b"factory");

        // MD5 entry: 2-byte magic, 14 reserved 0xFF bytes, 16-byte digest.
        data[32..34].copy_from_slice(&0xEBEB_u16.to_le_bytes());
        let digest = Md5::digest(&data[..32]);
        data[48..64].copy_from_slice(&digest);

        assert_eq!(parse_esp32_partition_table(&data).unwrap().len(), 1);

        let mut corrupted = data.clone();
        corrupted[48] ^= 0x01;
        assert!(parse_esp32_partition_table(&corrupted).is_err());

        let mut malformed = data.clone();
        malformed[34] = 0;
        assert!(parse_esp32_partition_table(&malformed).is_err());

        let mut changed_partition = data.clone();
        changed_partition[8] ^= 0x01;
        assert!(parse_esp32_partition_table(&changed_partition).is_err());

        let mut after_md5 = data.clone();
        after_md5[64..96].copy_from_slice(&data[..32]);
        assert!(parse_esp32_partition_table(&after_md5).is_err());
    }

    #[test]
    fn test_esp32_partition_table_parser() {
        use super::parse_esp32_partition_table;

        // Simulate a 3-entry partition table.
        let mut data = vec![0xFF_u8; 96];

        // Entry 1: Factory application partition.
        data[0..2].copy_from_slice(&0x50AA_u16.to_le_bytes());
        data[2] = 0x00; // Application type
        data[3] = 0x00; // Factory subtype
        data[4..8].copy_from_slice(&0x10000_u32.to_le_bytes());
        data[8..12].copy_from_slice(&0x100000_u32.to_le_bytes());
        data[12..28].fill(0);
        data[12..19].copy_from_slice(b"factory");

        // Entry 2: NVS data partition.
        let second = 32;
        data[second..second + 2].copy_from_slice(&0x50AA_u16.to_le_bytes());
        data[second + 2] = 0x01; // Data type
        data[second + 3] = 0x02; // NVS subtype
        data[second + 4..second + 8].copy_from_slice(&0x9000_u32.to_le_bytes());
        data[second + 8..second + 12].copy_from_slice(&0x6000_u32.to_le_bytes());
        data[second + 12..second + 28].fill(0);
        data[second + 12..second + 15].copy_from_slice(b"nvs");
        let partitions = parse_esp32_partition_table(&data).unwrap();

        assert_eq!(partitions.len(), 2);

        assert_eq!(partitions[0].label, "factory");
        assert_eq!(partitions[0].offset, 0x10000);
        assert_eq!(partitions[0].size, 0x100000);
        assert!(partitions[0].is_application());

        assert_eq!(partitions[1].label, "nvs");
        assert_eq!(partitions[1].offset, 0x9000);
        assert!(!partitions[1].is_application());

        // Invalid partition entry magic must be rejected.
        let mut invalid = data.clone();
        invalid[0] = 0x00;

        assert!(parse_esp32_partition_table(&invalid).is_err());
    }
    #[test]
    fn test_esp32_application_partition_validation() {
        use super::validate_esp32_application_partition;

        // Application partition: starts at 0x10000, size 1 MB.
        let partition_address = 0x10000;
        let partition_size = 1024 * 1024;

        // Valid: 512 KB firmware fits inside the partition.
        assert!(
            validate_esp32_application_partition(
                0x10000,
                512 * 1024,
                partition_address,
                partition_size,
            )
            .is_ok()
        );

        // Valid: firmware exactly fills the partition.
        assert!(
            validate_esp32_application_partition(
                0x10000,
                partition_size,
                partition_address,
                partition_size,
            )
            .is_ok()
        );

        // Invalid: firmware exceeds the partition.
        assert!(
            validate_esp32_application_partition(
                0x10000,
                2 * 1024 * 1024,
                partition_address,
                partition_size,
            )
            .is_err()
        );

        // Invalid: firmware starts before the partition.
        assert!(
            validate_esp32_application_partition(
                0x8000,
                512 * 1024,
                partition_address,
                partition_size,
            )
            .is_err()
        );

        // Invalid: zero-size firmware.
        assert!(
            validate_esp32_application_partition(0x10000, 0, partition_address, partition_size,)
                .is_err()
        );

        // Invalid: zero-size partition.
        assert!(
            validate_esp32_application_partition(0x10000, 512 * 1024, partition_address, 0,)
                .is_err()
        );
    }
    #[test]
    fn test_esp32_flash_size_parser() {
        // Standard ESP32 flash sizes.
        assert_eq!(
            super::parse_esp32_flash_size("Detected flash size: 4MB").unwrap(),
            4 * 1024 * 1024
        );

        assert_eq!(
            super::parse_esp32_flash_size("Detected flash size: 8MB").unwrap(),
            8 * 1024 * 1024
        );

        assert_eq!(
            super::parse_esp32_flash_size("Detected flash size: 16MB").unwrap(),
            16 * 1024 * 1024
        );

        // Smaller flash size.
        assert_eq!(
            super::parse_esp32_flash_size("Detected flash size: 512KB").unwrap(),
            512 * 1024
        );

        // Reject missing or invalid flash sizes.
        assert!(super::parse_esp32_flash_size("No flash detected").is_err());
        assert!(super::parse_esp32_flash_size("Detected flash size: 0MB").is_err());
        assert!(super::parse_esp32_flash_size("Detected flash size: unknown").is_err());
    }
    #[test]
    fn test_esp32_chip_model_parser() {
        let esp32 = "Chip is ESP32 (revision v3.0)";
        let esp32s3 = "Chip is ESP32-S3 (QFN56)";
        let esp32c3 = "Chip is ESP32-C3 (revision v0.4)";

        assert_eq!(super::parse_esp32_chip_model(esp32).unwrap(), "esp32");

        assert_eq!(super::parse_esp32_chip_model(esp32s3).unwrap(), "esp32s3");

        assert_eq!(super::parse_esp32_chip_model(esp32c3).unwrap(), "esp32c3");

        assert!(super::parse_esp32_chip_model("No chip detected").is_err());

        assert!(super::parse_esp32_chip_model("Chip is UNKNOWN").is_err());
    }
    #[test]
    fn test_esp32_chip_match_validation() {
        assert!(super::validate_esp32_chip_match("esp32s3", "esp32s3").is_ok());

        assert!(super::validate_esp32_chip_match("ESP32S3", "esp32s3").is_ok());

        assert!(super::validate_esp32_chip_match("esp32c3", "esp32s3").is_err());
    }
    #[test]
    fn test_esp32_firmware_chip_id_validation() {
        use std::io::Write;

        let path =
            std::env::temp_dir().join(format!("patchwork_chip_test_{}.bin", std::process::id()));

        let mut image = [0u8; 24];
        image[0] = 0xE9;
        image[1] = 1;

        // ESP32-S3 chip ID = 9, stored at bytes 12-13.
        image[12..14].copy_from_slice(&9u16.to_le_bytes());

        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&image).unwrap();
        drop(file);

        assert!(super::validate_esp32_firmware_chip(&path, "esp32s3").is_ok());

        assert!(super::validate_esp32_firmware_chip(&path, "esp32c3").is_err());

        std::fs::remove_file(path).unwrap();
    }
}
