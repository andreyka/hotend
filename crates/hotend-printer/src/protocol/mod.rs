//! The Bambu LAN message model: what the printer reports ([`report`]) and the only
//! requests Hotend can send ([`command`]).
//!
//! Sources, for every field name and bit position in these modules:
//! - OpenBambuAPI, `mqtt.md` (<https://raw.githubusercontent.com/Doridian/OpenBambuAPI/main/mqtt.md>):
//!   topics, the request and report envelope, `push_status`, `project_file`, `pause`,
//!   `resume`, `stop`, `print_speed`, `ledctrl`, `pushall`;
//! - ha-bambulab (MIT), `pybambu/models.py`, `const.py` and `commands.py`: the dual-nozzle
//!   `device.extruder` block, packed temperatures, AMS `info` bits, AMS HT ids, HMS codes,
//!   stage numbers;
//! - the sources named in `profiles/bambu.toml` for what differs on the H2 / X2 generation.
//!
//! What is taken are facts (names, numbers, spellings); no text or file of OpenBambuAPI
//! (GFDL) is in this tree. ha-bambulab names the same topics, ports and user name
//! (`bambu_client.py`), the same commands and light fields and the same keys of
//! `project_file` (`commands.py`), the same speed levels (`const.py`) and the same fields of
//! a report (`models.py`). Among the facts only OpenBambuAPI gives are: the field `sdcard`, the empty `param` of
//! `pause`, `resume` and `stop` and their QoS 1, `version` and `push_target` of `pushall`,
//! `file` and `md5` of `project_file`, and that the certificate names the serial number.
//!
//! None of it has been checked against a real X2D; model differences are data in
//! `profiles/bambu.toml`, each with `verified = false`.

pub mod command;
pub mod hms;
pub mod report;
