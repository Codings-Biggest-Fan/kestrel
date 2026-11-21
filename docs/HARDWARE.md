# Hardware notes

## EC access on Windows: MSI ACPI-WMI

- Namespace `root\wmi`, class `MSI_ACPI` (one instance). Needs admin, so only `kestrel-svc` calls it.
- `Get_Data(Data: Package_32)` / `Set_Data(Data: Package_32)`. `Package_32.Bytes` is `uint8[32]`.
  - Read: `Bytes[0] = addr`; the out-param `Data.Bytes[1]` is the value.
  - Write: `Bytes[0] = addr`, `Bytes[1] = value`.
- `Get_EC()` returns the EC firmware ID as ASCII in `Data.Bytes[2..]` (12 chars, e.g. `15MMIMS1.103`).
- The ACPI method behind it is not thread-safe: every call goes through one lock in the service.
- Some older firmware refuses the methods (WBEM_E_NOT_SUPPORTED); those boards need the
  PawnIO fallback (M6) and stay read-only until then.

## Model matching

The firmware ID minus its last two characters (`15MMIMS1.103` -> `15MMIMS1.1`) selects the
register map, as in msi-ec. Unknown IDs run read-only.

- `models/ec/*.toml`: generated from msi-ec by `tools/import_msi_ec.py`. Tier `experimental`.
- `models/verified/*.toml`: maps confirmed on owner hardware. Tier `tested`.

Experimental maps stay read-only until the user opts in for that exact firmware prefix.

## Write safety

Every EC write:
1. only targets a register named in the matched map;
2. checks the current value is one the map expects (a mode register holding an unknown byte
   means the map is wrong for this board, so nothing is written);
3. records the original value in the restore journal before the first write;
4. reads the register back and restores the original if it did not stick.

## Fans

- Tachometer: 16-bit big-endian divisor at `addr:addr+1`; `rpm = 480000 / raw` (0 = stopped).
- G2 fan-curve tables: CPU temps from 0x69, CPU speeds from 0x72, GPU temps from 0x81,
  GPU speeds from 0x8A, 6 points; fan mode `advanced` (0x8D) makes the EC follow them.

Sources: msi-ec (GPL-2.0-or-later), GhostDeck (GPL-3.0), Linux `msi-wmi-platform` docs.

## GIGABYTE AORUS/AERO laptops (experimental)

- Detected when `Win32_ComputerSystem.Manufacturer` contains `GIGABYTE` and `root\wmi` has
  `GB_WMIACPI_Get` / `GB_WMIACPI_Set` instances (installed by the BIOS, no vendor software needed).
- Getters return `Data`; setters take `Data` (byte). Fan curve: `GetFanIndexValue(Index)` returns
  `Temperture` (sic) and `Value`; `SetFanIndexValue(Index, Temperture, Value)`. 15 points, one curve
  for both fans, duty 0-229.
- Fan modes are four flags: `SetAutoFanStatus` (Gaming), `SetFixedFanStatus` (Fixed),
  `SetStepFanStatus` (Custom curve), `SetNvThermalTarget` (Quiet). Kestrel maps Extreme -> Gaming,
  Balanced -> Normal (all off), Silent/Super Battery -> Quiet, Cooler Boost -> Fixed at 229.
- Charge limit: `SetChargeStop(60-100)` then `SetChargePolicy(4)`; `SetChargePolicy(0)` charges to 100 %.
- Sensors: `getCpuTemp`, `getGpuTemp1`, `getRpm1`/`getRpm2` (byte-swapped u16), `GetFanPWMStatus` (0-229).
- Every write reads the matching getter first, journals it in `journal-gigabyte.json`, reads back
  and restores on mismatch. Curve writes are all-or-nothing.
- No GIGABYTE model is confirmed on owner hardware yet, so the backend is read-only until the owner
  adds `GIGABYTE:<SystemSKUNumber>` to `allow_experimental` in `%ProgramData%\Kestrel\settings.json`.

Sources: AeroCtl (GPL-3.0, method names and argument shapes), gigabyte-laptop-wmi (GPL-2.0-or-later,
fan modes, 15-point curve, 60-100 % charge range). Develop without hardware with
`kestrel-svc --mock gigabyte`.
