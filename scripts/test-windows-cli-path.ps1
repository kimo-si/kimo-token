$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-cli-path.ps1') -Mode Add -Scope User -Directory 'C:\Program Files\kimo-token\bin'

function Assert-Path([string]$Actual, [string]$Expected) {
    if ($Actual -cne $Expected) { throw "PATH mismatch: expected [$Expected], got [$Actual]" }
}

$bin = 'C:\Program Files\kimo-token\bin'
Assert-Path (Update-kimoTokenPath '' $bin Add) $bin
$original = '%SystemRoot%\system32;C:\Users\demo\other;'
$added = Update-kimoTokenPath $original $bin Add
Assert-Path $added ($original + ';' + $bin)
Assert-Path (Update-kimoTokenPath $added $bin Add) $added
Assert-Path (Update-kimoTokenPath $added $bin Remove) $original
Assert-Path (Update-kimoTokenPath ('C:\Users\demo\other;"c:\PROGRAM FILES\kimo-token\bin\";C:\Users\demo\last') $bin Add) 'C:\Users\demo\other;"c:\PROGRAM FILES\kimo-token\bin\";C:\Users\demo\last'
Assert-Path (Update-kimoTokenPath ('C:\Users\demo\other;"c:\PROGRAM FILES\kimo-token\bin\";C:\Users\demo\last') $bin Remove) 'C:\Users\demo\other;C:\Users\demo\last'
Assert-Path (Update-kimoTokenPath "$bin;$bin" $bin Remove) ''
Assert-Path (Update-kimoTokenPath 'C:\Program Files\kimo-token\bin-other;C:\Users\demo\other' $bin Remove) 'C:\Program Files\kimo-token\bin-other;C:\Users\demo\other'
Assert-Path (Update-kimoTokenPath 'C:\Users\demo\other;;C:\Users\demo\last;' $bin Remove) 'C:\Users\demo\other;;C:\Users\demo\last;'

# Exercise a PATH longer than NSIS's string buffer and keep variable references.
$longPath = ('%SystemRoot%\system32;' * 1000).TrimEnd(';')
Assert-Path (Update-kimoTokenPath (Update-kimoTokenPath $longPath $bin Add) $bin Remove) $longPath
$expanded = Join-Path $env:LOCALAPPDATA 'kimo-token\bin'
Assert-Path (Update-kimoTokenPath '%LOCALAPPDATA%\kimo-token\bin;C:\Users\demo\other' $expanded Add) '%LOCALAPPDATA%\kimo-token\bin;C:\Users\demo\other'
Assert-Path (Update-kimoTokenPath '%LOCALAPPDATA%\kimo-token\bin;C:\Users\demo\other' $expanded Remove) 'C:\Users\demo\other'
Write-Output 'Windows CLI PATH checks passed: idempotence, exact removal, quoted paths, variables, empty entries and long PATH.'
