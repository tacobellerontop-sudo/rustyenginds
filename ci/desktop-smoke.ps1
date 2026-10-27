# Runs the wallpaper on the CI runner's real desktop and reports what
# happened: shell window tree, sampled screen colors before/during/after,
# and the engine's log. Screenshots are uploaded as an artifact.
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
New-Item -ItemType Directory smoke -Force | Out-Null

function Shot($name) {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    try { $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size) }
    catch { "== $name screenshot failed: $_"; return }
    $bmp.Save("$PWD/smoke/$name.png")
    "== $name ($($b.Width)x$($b.Height)) sampled colors:"
    foreach ($fy in 0.1, 0.3, 0.5, 0.7, 0.9) {
        $row = foreach ($fx in 0.1, 0.3, 0.5, 0.7, 0.9) {
            $c = $bmp.GetPixel([int]($b.Width * $fx), [int]($b.Height * $fy))
            '{0:x2}{1:x2}{2:x2}' -f $c.R, $c.G, $c.B
        }
        "   " + ($row -join ' ')
    }
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

foreach ($mode in 'gdi', 'ulw') {
    "#################### present mode: $mode"
    $env:RUSTY_WALLPAPER_PRESENT = $mode
    $p = Start-Process ./dist/rusty-wallpaper.exe -ArgumentList '--background', '--config', $cfg -PassThru
    Start-Sleep 8
    "== engine still running after 8s: $(-not $p.HasExited)"
    Shot "during-$mode"
    ./dist/rusty-wallpaper.exe stop
    Start-Sleep 3
    "== engine exited: $($p.HasExited) (code $($p.ExitCode))"
    Shot "after-$mode"
    "== engine log ($mode):"
    Get-Content "$env:LOCALAPPDATA/rusty-wallpaper/rusty-wallpaper.log"
}
