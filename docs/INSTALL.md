# Installing and testing Kestrel on Windows

Kestrel is early software. These builds are **not code-signed**, so Windows warns before running them.

## 1. Download

1. Open the [Releases page](https://github.com/Codings-Biggest-Fan/kestrel/releases) and pick the newest release.
2. Under **Assets**, download the `...-windows-x64.zip` file.
3. Right-click the zip → **Extract All…** → **Extract**. You get a `Kestrel` folder holding
   `kestrel-svc.exe`, `kestrel-ui.exe` and `kestrel-tray.exe`.

## Quick start

1. Close MSI Center (see step 2).
2. Double-click **`Start Kestrel.cmd`** in the extracted folder and click **Yes** when Windows asks for
   administrator rights. If SmartScreen appears: **More info** → **Run anyway**.
3. A console window titled *Kestrel service* opens (it prints every change; close it to stop Kestrel),
   then the tray icon and the main window.

Steps 3–6 below do the same thing by hand.

## 2. Close MSI Center

Two programs writing to the embedded controller (EC) at the same time can interfere with each other.
Click the **^** arrow next to the clock, right-click **MSI Center** → **Exit**.

## 3. Open PowerShell as administrator

The service needs administrator rights to talk to MSI's firmware interface. Without them you get
`MSI_ACPI access denied: run kestrel-svc as administrator`.

1. Press the **Windows key** and type `powershell`.
2. Click **Run as administrator** on the right (or right-click **Windows PowerShell** → **Run as administrator**).
3. Click **Yes** on the User Account Control prompt.
4. Check that the window title starts with **Administrator:**.

## 4. Go to the Kestrel folder

1. In File Explorer, open the extracted `Kestrel` folder, click the address bar and copy the path.
2. In PowerShell, type `cd "`, paste the path, type `"` and press **Enter**:

   ```powershell
   cd "C:\Users\YourName\Downloads\Kestrel-preview-5-windows-x64\Kestrel"
   ```

## 5. Start the service

```powershell
.\kestrel-svc.exe --console
```

- If Windows SmartScreen appears: **More info** → **Run anyway**.
- The first line shows what was detected, for example
  `kestrel-svc 0.1.0: firmware "15MMIMS1.103" map Some("VECTOR_A16_HX_A8W") read_only=false`.
  `read_only=true` means your model has no verified map yet; Kestrel shows readings but changes nothing.
- Leave this window open. Each change you make is printed with the resulting register values, e.g.
  `SetCoolerBoost(false) -> Ok | 0x98=0x02 0xd2=0xc1 0xd4=0x0d`.

## 6. Open the app

Double-click `kestrel-ui.exe` (the main window) and optionally `kestrel-tray.exe` (tray icon with quick
scenario switching). Closing the main window exits it completely.

## 7. Undo everything

- **Settings → Restore original EC values** writes back every register Kestrel changed.
- Stop the service with **Ctrl+C** in the PowerShell window.

## Reporting a problem or a new laptop

Open an [issue](https://github.com/Codings-Biggest-Fan/kestrel/issues) with your laptop model, the first
line printed by `kestrel-svc.exe --console`, and any lines printed when the problem happened.
