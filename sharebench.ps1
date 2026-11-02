# Echo share-link speed test, round 3: which encoder setup is fastest (30 s of one clip).
param([string]$Clip = "")
if (-not $Clip) {
  $Clip = Get-ChildItem -Path "$HOME\Videos", "$HOME\Desktop" -Recurse -Filter "FiveM 2026-11-02 17-51-32.mp4" -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -notmatch "\\\.thumbs\\" } | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $Clip -or -not (Test-Path $Clip)) { Write-Host "Clip not found. Run: .\sharebench.ps1 -Clip 'C:\path\to\clip.mp4'"; exit }
$ff = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if (-not $ff) { Write-Host "ffmpeg.exe not found"; exit }

$wm = "$env:APPDATA\Clipper\watermark.apng"
if (-not (Test-Path $wm)) { $wm = "$env:APPDATA\Clipper\watermark_echo.apng" }
$wmf = ($wm -replace '\\', '/') -replace ':', '\:'
$o = "$env:TEMP\echo_bench"
$tv = "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p"
$wmg = "movie='$wmf':f=apng,format=rgba,scale=-1:81[w];[in][w]overlay=W-w-27:H-h-27:format=yuv420,$tv"
$amf = @("-c:v", "h264_amf", "-quality", "speed", "-b:v", "8000k")

$results = @()
function Add($name, $secs, $err) {
  $line = "{0,-40} {1,6:N1} s" -f $name, $secs
  if ($err) { $line += "   ERROR: $err" }
  $script:results += $line
}
function T($name, [string[]]$a) {
  Write-Host "running: $name"
  $s = Measure-Command { $script:err = (& $ff -hide_banner -loglevel error -y @a 2>&1 | ForEach-Object { "$_" } |
    Where-Object { $_ -notmatch "monotonically" } | Select-Object -First 2) -join " | " }
  Add $name $s.TotalSeconds $script:err
}
# runs several ffmpeg at once (each its own part of the clip) and times until all are done
function P($name, [string[][]]$jobs) {
  Write-Host "running: $name"
  $t = [Diagnostics.Stopwatch]::StartNew()
  $ps = foreach ($j in $jobs) {
    $q = ($j | ForEach-Object { if ($_ -match '[\s;,\[\]]') { '"' + $_ + '"' } else { $_ } }) -join ' '
    Start-Process -FilePath $ff -ArgumentList "-hide_banner -loglevel error -y $q" -NoNewWindow -PassThru
  }
  $ps | ForEach-Object { $_.WaitForExit() }
  $bad = ($ps | Where-Object { $_.ExitCode -ne 0 }).Count
  Add $name $t.Elapsed.TotalSeconds $(if ($bad) { "$bad of $($ps.Count) failed" } else { "" })
}

T "A AMF speed + watermark (now)"        (@("-t", "30", "-i", $Clip, "-an", "-vf", $wmg) + $amf + @("$o.a.mp4"))
T "B AMF, no B-frames/preanalysis"       (@("-t", "30", "-i", $Clip, "-an", "-vf", $wmg) + $amf + @("-bf", "0", "-preanalysis", "false", "$o.b.mp4"))
T "C AMF all on GPU (no watermark)"      @("-hwaccel", "d3d11va", "-hwaccel_output_format", "d3d11", "-t", "30", "-i", $Clip, "-an", "-c:v", "h264_amf", "-quality", "speed", "-b:v", "8000k", "$o.c.mp4")
T "D CPU x264 superfast + watermark"     @("-t", "30", "-i", $Clip, "-an", "-vf", $wmg, "-c:v", "libx264", "-preset", "superfast", "-b:v", "8000k", "-maxrate", "8000k", "-bufsize", "16000k", "$o.d.mp4")
T "E CPU x264 veryfast + watermark"      @("-t", "30", "-i", $Clip, "-an", "-vf", $wmg, "-c:v", "libx264", "-preset", "veryfast", "-b:v", "8000k", "-maxrate", "8000k", "-bufsize", "16000k", "$o.e.mp4")
T "F AMF 30 fps + watermark"             (@("-t", "30", "-i", $Clip, "-an", "-vf", "fps=30,$wmg") + $amf + @("$o.f.mp4"))
P "G 2x AMF at once (2 halves)"          @(
  (@("-ss", "0",  "-t", "15", "-i", $Clip, "-an", "-vf", $wmg) + $amf + @("$o.g1.mp4")),
  (@("-ss", "15", "-t", "15", "-i", $Clip, "-an", "-vf", $wmg) + $amf + @("$o.g2.mp4")))
P "H 3x AMF at once (3 thirds)"          @(
  (@("-ss", "0",  "-t", "10", "-i", $Clip, "-an", "-vf", $wmg) + $amf + @("$o.h1.mp4")),
  (@("-ss", "10", "-t", "10", "-i", $Clip, "-an", "-vf", $wmg) + $amf + @("$o.h2.mp4")),
  (@("-ss", "20", "-t", "10", "-i", $Clip, "-an", "-vf", $wmg) + $amf + @("$o.h3.mp4")))
Remove-Item "$o.*" -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "==== RESULTS (30 s of clip) ===="
$results | ForEach-Object { Write-Host $_ }
Write-Host ""
Write-Host "Send a screenshot of the RESULTS block to Claude."
