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

"== desktop before:"
./dist/rusty-wallpaper.exe desktop-info
Shot before

$p = Start-Process ./dist/rusty-wallpaper.exe -ArgumentList '--background', '--scene', 'synthwave' -PassThru
Start-Sleep 8
"== engine still running after 8s: $(-not $p.HasExited)"
"== desktop during:"
./dist/rusty-wallpaper.exe desktop-info
Shot during

./dist/rusty-wallpaper.exe stop
Start-Sleep 3
"== engine exited: $($p.HasExited) (code $($p.ExitCode))"
Shot after

"== engine log:"
Get-Content "$env:LOCALAPPDATA/rusty-wallpaper/rusty-wallpaper.log"
