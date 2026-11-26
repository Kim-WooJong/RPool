#Requires -Version 7.3
# Disposable GitHub-hosted runner data only; never uses the operator's pools.
$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
$root=Join-Path $env:RUNNER_TEMP 'rpool-winfsp-smoke'
if(Test-Path $root){throw 'Smoke directory already exists'}
New-Item $root -ItemType Directory|Out-Null
$exe=(Resolve-Path 'target/release/rpool.exe').Path
$rclone=$env:RPOOL_TEST_RCLONE
if(-not(Test-Path $rclone)){throw 'Pinned rclone missing'}
Add-Type -Namespace RPoolSmoke -Name Native -MemberDefinition '[DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern uint GetFileAttributesW(string path); [DllImport("kernel32.dll")] public static extern uint GetLogicalDrives();'
if(([RPoolSmoke.Native]::GetLogicalDrives() -band (1 -shl 17)) -ne 0){throw 'R: is already in use'}
$env:RPOOL_CONFIG_DIR=Join-Path $root 'config'
$env:RCLONE_CONFIG=Join-Path $root 'rclone.conf'
New-Item $env:RPOOL_CONFIG_DIR -ItemType Directory|Out-Null
$lines=@()
for($i=1;$i -le 3;$i++){
 $data=Join-Path $root ('data'+$i);New-Item $data -ItemType Directory|Out-Null
 $password=(& $rclone obscure ('public-ci-fixture-'+$i)|Out-String).Trim()
 if($LASTEXITCODE){throw 'Fixture key creation failed'}
 $lines+=@('[b'+$i+']','type = local','','[c'+$i+']','type = crypt',('remote = b'+$i+':'+$data.Replace('\','/')),'filename_encryption = standard','directory_name_encryption = true','filename_encoding = base32',('password = '+$password),'')
}
[IO.File]::WriteAllLines($env:RCLONE_CONFIG,$lines,[Text.UTF8Encoding]::new($false))
function InvokeCli([string[]]$Arguments){
 $info=[Diagnostics.ProcessStartInfo]::new($exe);$info.UseShellExecute=$false;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
 foreach($a in $Arguments){$info.ArgumentList.Add($a)}
 $p=[Diagnostics.Process]::Start($info);$o=$p.StandardOutput.ReadToEndAsync();$e=$p.StandardError.ReadToEndAsync()
 if(-not $p.WaitForExit(60000)){$p.Kill($true);$p.WaitForExit();throw 'Owned fixture CLI timed out'}
 $text=$o.GetAwaiter().GetResult();$errorText=$e.GetAwaiter().GetResult();if($p.ExitCode){throw ('Fixture CLI failed: '+$errorText)};return $text
}
$null=InvokeCli @('--rclone',$rclone,'pool','set','ci','--remote','c1:','--remote','c2:','--remote','c3:','--shard-mib','1','--data-shards','2','--parity-shards','1','--native-crypt','--small-file-packing')
$mounts=[Collections.Generic.List[object]]::new()
function StartSmokeMount([string]$Phase){
 $workspace=Join-Path $root ('workspace-'+$Phase);$stop=Join-Path $root ($Phase+'.stop')
 $a=@('--rclone',$rclone,'mount','--pool','ci','--workspace',$workspace,'--mountpoint','R:','--frontend','winfsp','--interval-seconds','2','--stop-file',$stop)
 $info=[Diagnostics.ProcessStartInfo]::new($exe);$info.UseShellExecute=$false;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
 foreach($s in $a){$info.ArgumentList.Add($s)}
 $p=[Diagnostics.Process]::Start($info)
 $m=[pscustomobject]@{Process=$p;Output=$p.StandardOutput.ReadToEndAsync();Error=$p.StandardError.ReadToEndAsync();Stop=$stop;Workspace=$workspace;Phase=$Phase}
 $mounts.Add($m);$end=[DateTime]::UtcNow.AddSeconds(90)
 while([DateTime]::UtcNow -lt $end){if($p.HasExited){throw 'Native mount exited before ready'};$attributes=[RPoolSmoke.Native]::GetFileAttributesW('R:\');if($attributes -ne [uint32]::MaxValue -and ($attributes -band 0x10)){return $m};Start-Sleep -Milliseconds 200}
 throw 'Native WinFsp mount readiness timed out'
}
function StopSmokeMount($m){
 if(-not $m.Process.HasExited){[IO.File]::WriteAllText($m.Stop,'normal fixture shutdown');if(-not $m.Process.WaitForExit(90000)){throw 'Native mount normal shutdown timed out'}}
 if(-not $m.Output.Wait(10000) -or -not $m.Error.Wait(10000)){throw 'Mount output pipe did not close'}
 [IO.File]::WriteAllText((Join-Path $root ($m.Phase+'-stdout.log')),$m.Output.Result)
 [IO.File]::WriteAllText((Join-Path $root ($m.Phase+'-stderr.log')),$m.Error.Result)
 if($m.Process.ExitCode){throw 'Native mount shutdown exit was nonzero'}
 $end=[DateTime]::UtcNow.AddSeconds(10);while((([RPoolSmoke.Native]::GetLogicalDrives() -band (1 -shl 17)) -ne 0) -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
 if(([RPoolSmoke.Native]::GetLogicalDrives() -band (1 -shl 17)) -ne 0){throw 'Native mountpoint survived shutdown'}
}
function WriteFixture([string]$Path,[byte[]]$Bytes){$f=[IO.File]::Open($Path,[IO.FileMode]::CreateNew);try{$f.Write($Bytes,0,$Bytes.Length);$f.Flush($true)}finally{$f.Dispose()}}
function AssertBytes([string]$Path,[byte[]]$Expected){$actual=[IO.File]::ReadAllBytes($Path);$sha=[Security.Cryptography.SHA256]::HashData($actual);$want=[Security.Cryptography.SHA256]::HashData($Expected);if(-not [Linq.Enumerable]::SequenceEqual[byte]($sha,$want)){throw ('Fixture content mismatch: '+[IO.Path]::GetFileName($Path))}}
function WaitSynced($m,[int]$ExpectedCount){
 $end=[DateTime]::UtcNow.AddSeconds(120)
 while([DateTime]::UtcNow -lt $end){
  if($m.Process.HasExited){throw 'Mount exited while syncing'}
  $view=(InvokeCli @('--rclone',$rclone,'pool','browse','ci','--json'))|ConvertFrom-Json
  if(-not $view.PSObject.Properties['entries']){throw 'Invalid browse result shape'}
  $n=Join-Path $m.Workspace '.rpool/net-status.json'
  if(Test-Path $n){$q=([IO.File]::ReadAllText($n)|ConvertFrom-Json).queue;if($view.entries.Count -eq $ExpectedCount -and $q.pending_files -eq 0 -and $q.pending_bytes -eq 0){return}}
  Start-Sleep -Seconds 1
 }
 throw 'Fixture cloud namespace did not finish synchronizing'
}
try{
 $m=StartSmokeMount 'first'
 [byte[]]$big=New-Object byte[] (3*1024*1024+37);[Random]::new(42).NextBytes($big)
 [byte[]]$small=[Text.Encoding]::UTF8.GetBytes('native WinFsp local-only verification')
 WriteFixture 'R:\big.bin' $big;WriteFixture 'R:\small.txt' $small;WriteFixture 'R:\empty.bin' ([byte[]]@());WriteFixture 'R:\remove.txt' $small
 [IO.File]::Move('R:\small.txt','R:\renamed.txt');[IO.File]::Delete('R:\remove.txt')
 $f=[IO.File]::Open('R:\big.bin',[IO.FileMode]::Open,[IO.FileAccess]::Write);try{$f.SetLength(1048579);$f.Flush($true)}finally{$f.Dispose()}
 [byte[]]$trim=$big[0..1048578];AssertBytes 'R:\big.bin' $trim;AssertBytes 'R:\renamed.txt' $small;AssertBytes 'R:\empty.bin' ([byte[]]@())
 WaitSynced $m 3
 foreach($i in 1..3){$objects=@(Get-ChildItem (Join-Path $root ('data'+$i)) -Recurse -File);if(-not $objects.Count){throw 'Fixture replica has no encrypted objects'};if(@($objects|Where-Object {$_.Name -in @('big.bin','renamed.txt','empty.bin')}).Count){throw 'Plaintext fixture names found in crypt backend'}}
 StopSmokeMount $m
 $m=StartSmokeMount 'fresh-peer'
 AssertBytes 'R:\big.bin' $trim;AssertBytes 'R:\renamed.txt' $small;AssertBytes 'R:\empty.bin' ([byte[]]@())
 if([IO.File]::Exists('R:\small.txt') -or [IO.File]::Exists('R:\remove.txt')){throw 'Deleted or renamed source returned'}
 [IO.File]::Delete('R:\big.bin');[IO.File]::Delete('R:\renamed.txt');[IO.File]::Delete('R:\empty.bin')
 WaitSynced $m 0;StopSmokeMount $m
 Write-Output 'WINFSP_LOCAL_IO_OK: create/read/truncate/rename/delete/empty/fresh-peer/normal-stop'
}finally{
 foreach($m in $mounts){if(-not $m.Process.HasExited){try{[IO.File]::WriteAllText($m.Stop,'fixture cleanup');if(-not $m.Process.WaitForExit(15000)){$m.Process.Kill($true);$m.Process.WaitForExit()}}catch{Write-Warning 'Owned fixture cleanup failed'}}}
 foreach($m in $mounts){foreach($pair in @(@('stdout',$m.Output),@('stderr',$m.Error))){if($pair[1].IsCompletedSuccessfully){$text=$pair[1].Result;[IO.File]::WriteAllText((Join-Path $root ($m.Phase+'-'+$pair[0]+'.log')),$text);if($text.Length){Write-Host ($m.Phase+' '+$pair[0]+': '+$text.Substring([Math]::Max(0,$text.Length-4000)))}}}}
}
