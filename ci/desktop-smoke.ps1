# Runs the wallpaper on the CI runner's real desktop and reports what
# happened: shell window tree, sampled screen colors before/during/after,
# and the engine's log.
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory smoke -Force | Out-Null

# Screen capture through BitBlt with CAPTUREBLT, which (unlike the
# CopyFromScreen default) includes layered windows such as ours.
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class ScreenGrab {
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr hwnd);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr hwnd, IntPtr dc);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int index);
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int w, int h);
    [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr obj);
    [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr dst, int x, int y, int w, int h, IntPtr src, int sx, int sy, uint rop);
    [DllImport("gdi32.dll")] static extern uint GetPixel(IntPtr dc, int x, int y);
    [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr obj);

    // Returns rows of "rrggbb" samples on a 5x5 grid.
    public static string[] Sample() {
        int w = GetSystemMetrics(0), h = GetSystemMetrics(1);
        IntPtr screen = GetDC(IntPtr.Zero);
        IntPtr mem = CreateCompatibleDC(screen);
        IntPtr bmp = CreateCompatibleBitmap(screen, w, h);
        IntPtr old = SelectObject(mem, bmp);
        BitBlt(mem, 0, 0, w, h, screen, 0, 0, 0x00CC0020 | 0x40000000); // SRCCOPY | CAPTUREBLT
        double[] f = { 0.1, 0.3, 0.5, 0.7, 0.9 };
        string[] rows = new string[6];
        rows[0] = w + "x" + h;
        for (int r = 0; r < 5; r++) {
            string row = "";
            for (int c = 0; c < 5; c++) {
                uint p = GetPixel(mem, (int)(w * f[c]), (int)(h * f[r]));
                row += string.Format("{0:x2}{1:x2}{2:x2} ", p & 0xFF, (p >> 8) & 0xFF, (p >> 16) & 0xFF);
            }
            rows[r + 1] = row;
        }
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(IntPtr.Zero, screen);
        return rows;
    }
}
"@

function Shot($name) {
    $rows = [ScreenGrab]::Sample()
    "== $name ($($rows[0])) sampled colors:"
    $rows[1..5] | ForEach-Object { "   $_" }
}

$cv = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
"== OS: $($cv.ProductName) $($cv.DisplayVersion) build $($cv.CurrentBuild)"
"== explorer: " + ((Get-Process explorer -ErrorAction SilentlyContinue | ForEach-Object { "pid $($_.Id) session $($_.SessionId)" }) -join ', ')
"== this session: $((Get-Process -Id $PID).SessionId)"

# Show the bare desktop: the runner's console window otherwise covers it.
(New-Object -ComObject Shell.Application).MinimizeAll()
Start-Sleep 2

# Bright scene, and no pausing behind other windows, so any pixels we
# manage to put on the desktop are unmistakable.
$cfg = "$PWD/smoke/config.toml"
"scene = `"plasma`"`npause_when_covered = false`n" | Set-Content $cfg

"== desktop before:"
./dist/rusty-wallpaper.exe desktop-info
Shot before

# placement / present-mode combinations. "none" is the control: a plain
# topmost window, proving the sampling can see our pixels at all.
$trials = @(
    @('none', 'gdi'),
    @('default', 'gdi'),
    @('default', 'ulw'),
    @('progman-top', 'gdi'),
    @('worker', 'gdi')
)
foreach ($t in $trials) {
    $attach, $mode = $t
    "#################### placement: $attach, present: $mode"
    $env:RUSTY_WALLPAPER_ATTACH = $(if ($attach -eq 'default') { '' } else { $attach })
    $env:RUSTY_WALLPAPER_PRESENT = $mode
    $p = Start-Process ./dist/rusty-wallpaper.exe -ArgumentList '--background', '--config', $cfg -PassThru
    Start-Sleep 6
    "== engine still running: $(-not $p.HasExited)"
    Shot "during $attach/$mode"
    ./dist/rusty-wallpaper.exe stop
    Start-Sleep 2
    "== engine log:"
    Get-Content "$env:LOCALAPPDATA/rusty-wallpaper/rusty-wallpaper.log" | Select-String -Pattern 'presenting|attached|first frame|rusty-wallpaper \(|ERROR|WARN'
}
