#requires -Version 7.0
<#
只读重跑真实 NTFS 卷验收；输出只进入本次新建的临时目录。
示例：pwsh -File .\crates\diskmap-core\tests\fixtures\ntfs\accept-volumes.ps1 -Volume C:,D:
扫描与对账不是快照。无法证实的差额保留在 other，不用路径名字猜测硬链接或 ADS。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string[]]$Volume,
    [string]$ScanExe = (Join-Path $PSScriptRoot '..\..\..\..\..\target\release\examples\scan.exe'),
    [switch]$Apparent
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw '此脚本需要 Windows / This script requires Windows.' }
$ScanExe = (Resolve-Path -LiteralPath $ScanExe).Path
$Roots = [Collections.Generic.List[string]]::new()
foreach ($value in ($Volume -split ',')) {
    if ($value -notmatch '^([A-Za-z]):[\\/]?$') {
        throw "只接受本地盘符，例如 C: / Expected a local drive letter: $value"
    }
    $root = $Matches[1].ToUpperInvariant() + ':\'
    if (-not $Roots.Contains($root)) { $Roots.Add($root) }
}
$OutputDir = Join-Path ([IO.Path]::GetTempPath()) ('diskmap-ntfs-accept-' + [guid]::NewGuid())
# GUID 目录不能复用；所有文件均用 CreateNew，绝不覆盖用户文件。
if ([IO.Directory]::Exists($OutputDir)) { throw '临时目录已存在 / Temporary directory already exists.' }
[void][IO.Directory]::CreateDirectory($OutputDir)
Write-Host "输出目录 / Output directory: $OutputDir"

function New-OutputWriter([string]$Name) {
    $path = Join-Path $OutputDir $Name
    $file = [IO.FileStream]::new($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    return [IO.StreamWriter]::new($file, [Text.UTF8Encoding]::new($false))
}
function Write-OutputJson([string]$Name, $Value) {
    $writer = New-OutputWriter $Name
    try { $writer.WriteLine(($Value | ConvertTo-Json -Depth 12)) } finally { $writer.Dispose() }
}
function Get-VolumeStamp([string]$Root) {
    $drive = [IO.DriveInfo]::new($Root)
    [pscustomobject]@{ utc = [DateTime]::UtcNow.ToString('o'); root = $Root;
        format = $drive.DriveFormat; total_bytes = $drive.TotalSize; free_bytes = $drive.TotalFreeSpace }
}

# 只请求 FILE_READ_ATTRIBUTES，且用 OPEN_NO_RECALL/OPEN_REPARSE_POINT；不打开文件数据、不启用权限。
# 结构与参数依据 Microsoft 的 FILE_STREAM_INFO、FindFirstFileNameW 和 CreateFileW 文档。
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace DiskMapAcceptance {
    public sealed class Info {
        public string Id;
        public ulong Reference;
        public uint Attributes;
        public uint Links;
        public int Error;
        public int StandardError;
        public bool StandardValid;
        public long StandardAllocated;
        public long StandardSize;
        public int StreamError;
        public bool StreamsValid;
        public long DefaultAllocated;
        public long DefaultSize;
        public long AdsAllocated;
        public long AdsSize;
    }
    public static class Metadata {
        [StructLayout(LayoutKind.Sequential)]
        struct ByHandle {
            public uint Attributes;
            public System.Runtime.InteropServices.ComTypes.FILETIME Creation, Access, Write;
            public uint VolumeSerial, SizeHigh, SizeLow, Links, IndexHigh, IndexLow;
        }
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        static extern SafeFileHandle CreateFileW(string path, uint access, uint sharing, IntPtr security,
            uint disposition, uint flags, IntPtr template);
        [DllImport("kernel32.dll", SetLastError=true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        static extern bool GetFileInformationByHandle(SafeFileHandle file, out ByHandle info);
        [DllImport("kernel32.dll", SetLastError=true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        static extern bool GetFileInformationByHandleEx(SafeFileHandle file, int kind, IntPtr buffer, uint size);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        static extern IntPtr FindFirstFileNameW(string path, uint flags, ref uint length, StringBuilder name);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        static extern bool FindNextFileNameW(IntPtr handle, ref uint length, StringBuilder name);
        [DllImport("kernel32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        static extern bool FindClose(IntPtr handle);
        static string LongPath(string path) { return path.StartsWith(@"\\?\") ? path : @"\\?\" + path; }

        public static Info Read(string path, bool streams) {
            var result = new Info();
            using (var handle = CreateFileW(LongPath(path), 0x80, 7, IntPtr.Zero, 3, 0x02300000, IntPtr.Zero)) {
                if (handle.IsInvalid) { result.Error = Marshal.GetLastWin32Error(); return result; }
                ByHandle info;
                if (!GetFileInformationByHandle(handle, out info)) {
                    result.Error = Marshal.GetLastWin32Error(); return result;
                }
                result.Id = info.VolumeSerial.ToString("x8") + ":" + info.IndexHigh.ToString("x8") + info.IndexLow.ToString("x8");
                result.Reference = ((ulong)info.IndexHigh << 32) | info.IndexLow;
                result.Attributes = info.Attributes;
                result.Links = info.Links;
                if (!streams) return result;
                IntPtr standard = Marshal.AllocHGlobal(24);
                try {
                    if (GetFileInformationByHandleEx(handle, 1, standard, 24)) {
                        result.StandardAllocated = Marshal.ReadInt64(standard, 0);
                        result.StandardSize = Marshal.ReadInt64(standard, 8);
                        result.StandardValid = result.StandardAllocated >= 0 && result.StandardSize >= 0;
                    } else { result.StandardError = Marshal.GetLastWin32Error(); }
                } finally { Marshal.FreeHGlobal(standard); }
                for (int capacity = 4096; capacity <= 16 * 1024 * 1024; capacity *= 2) {
                    IntPtr buffer = Marshal.AllocHGlobal(capacity);
                    try {
                        if (!GetFileInformationByHandleEx(handle, 7, buffer, (uint)capacity)) {
                            result.StreamError = Marshal.GetLastWin32Error();
                            if (result.StreamError == 122 || result.StreamError == 234) continue;
                            return result;
                        }
                        long defaultAllocated=0, defaultSize=0, adsAllocated=0, adsSize=0;
                        int offset=0;
                        while (true) {
                            if (offset < 0 || offset > capacity-24) throw new InvalidOperationException("invalid stream offset");
                            IntPtr item = IntPtr.Add(buffer, offset);
                            uint next = unchecked((uint)Marshal.ReadInt32(item, 0));
                            uint nameBytes = unchecked((uint)Marshal.ReadInt32(item, 4));
                            if ((nameBytes & 1) != 0 || nameBytes > capacity-offset-24) throw new InvalidOperationException("invalid stream name");
                            string name = Marshal.PtrToStringUni(IntPtr.Add(item, 24), (int)nameBytes/2);
                            long size = Marshal.ReadInt64(item, 8), allocated = Marshal.ReadInt64(item, 16);
                            if (size < 0 || allocated < 0) throw new InvalidOperationException("negative stream size");
                            checked {
                                if (name == "::$DATA" || name.Length == 0) { defaultAllocated += allocated; defaultSize += size; }
                                else if (name.EndsWith(":$DATA", StringComparison.OrdinalIgnoreCase)) { adsAllocated += allocated; adsSize += size; }
                            }
                            if (next == 0) break;
                            if (next < 24 || (next & 7) != 0 || next > capacity-offset-24) throw new InvalidOperationException("invalid next stream");
                            offset = checked(offset+(int)next);
                        }
                        result.DefaultAllocated=defaultAllocated; result.DefaultSize=defaultSize;
                        result.AdsAllocated=adsAllocated; result.AdsSize=adsSize;
                        result.StreamError=0; result.StreamsValid=true;
                        return result;
                    } finally { Marshal.FreeHGlobal(buffer); }
                }
                return result;
            }
        }
        public static string[] HardLinks(string path) {
            var names = new List<string>();
            var buffer = new StringBuilder(32768);
            uint size=32768;
            IntPtr handle = FindFirstFileNameW(LongPath(path), 0, ref size, buffer);
            if (handle == new IntPtr(-1)) return names.ToArray();
            try {
                names.Add(buffer.ToString());
                // NTFS 最多 1024 个名字；另设硬上界，避免活动卷持续新增造成无界枚举。
                for (int i=1; i<4096; ++i) {
                    size=32768; buffer.Clear();
                    if (!FindNextFileNameW(handle, ref size, buffer)) break;
                    names.Add(buffer.ToString());
                }
            } finally { FindClose(handle); }
            return names.ToArray();
        }
    }

    public sealed class JournalPosition {
        public ulong Id;
        public long First, Next, LowestValid;
        public int Error;
    }
    public sealed class Change {
        public ulong Reference, Parent;
        public long Usn, FileTime;
        public uint Reason, EvidenceReason;
        public string Name;
    }
    public sealed class JournalEvidence {
        public bool Complete;
        public string Failure;
        public long BytesRead;
        public int RecordsRead, UnsupportedRecords;
        public ulong JournalId;
        public long StartUsn, EndUsn;
        readonly Dictionary<ulong,List<Change>> byFile = new Dictionary<ulong,List<Change>>();
        readonly Dictionary<string,List<Change>> byName = new Dictionary<string,List<Change>>(StringComparer.OrdinalIgnoreCase);
        readonly Dictionary<ulong,uint> previousReasons = new Dictionary<ulong,uint>();
        static string NameKey(ulong parent, string name) { return parent.ToString("x16") + "\\" + name; }
        public bool HasReference(ulong reference) { return Complete && byFile.ContainsKey(reference); }
        internal void Observe(Change change, bool inTimeWindow) {
            // 非 CLOSE 记录也累计打开以来的原因。只有同 FRN 相邻记录新增加的位能证明新事件。
            // 首条未知基线（或原因无 CLOSE 却减少）仅接受单一位；多位无法确定哪个刚刚发生。
            uint previous;
            bool known=previousReasons.TryGetValue(change.Reference, out previous);
            if ((change.Reason & 0x80000000) == 0) {
                if (known && (previous & 0x80000000) != 0) change.EvidenceReason=change.Reason;
                else if (known && (previous & ~change.Reason) == 0) change.EvidenceReason=change.Reason & ~previous;
                else if (change.Reason != 0 && (change.Reason & (change.Reason-1)) == 0) change.EvidenceReason=change.Reason;
            }
            previousReasons[change.Reference]=change.Reason;
            if (!inTimeWindow || change.EvidenceReason == 0) return;
            List<Change> list;
            if (!byFile.TryGetValue(change.Reference, out list)) {
                list = new List<Change>(); byFile.Add(change.Reference, list);
            }
            list.Add(change);
            string key = NameKey(change.Parent, change.Name);
            if (!byName.TryGetValue(key, out list)) { list = new List<Change>(); byName.Add(key, list); }
            list.Add(change);
        }
        public string Match(ulong reference, ulong parent, string name, uint reasons, bool requireName) {
            if (!Complete) return null;
            List<Change> list;
            bool found = requireName ? byName.TryGetValue(NameKey(parent, name), out list) : byFile.TryGetValue(reference, out list);
            if (!found) return null;
            for (int i=list.Count-1; i>=0; --i) {
                Change change = list[i];
                uint applicable=change.EvidenceReason & reasons;
                if (applicable == 0) continue;
                if (reference != 0 && change.Reference != reference) continue;
                return "USN journal=" + JournalId.ToString("x16") + " usn=" + change.Usn
                    + " utc=" + DateTime.FromFileTimeUtc(change.FileTime).ToString("o")
                    + " reason=0x" + change.Reason.ToString("x8") + " new_reason=0x" + change.EvidenceReason.ToString("x8")
                    + " file=" + change.Reference.ToString("x16")
                    + " parent=" + change.Parent.ToString("x16") + " name=" + change.Name;
            }
            return null;
        }
    }
    public static class Journal {
        // 仅 QUERY/READ；不创建、不调整、不删除 USN journal，也不写入卷。
        const uint QueryControl = 0x000900f4, ReadControl = 0x000900bb;
        const long MaxBytes = 64L * 1024 * 1024;
        const int MaxRecords = 200000;
        static readonly Encoding NameEncoding = new UnicodeEncoding(false, false, true);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        static extern SafeFileHandle CreateFileW(string path, uint access, uint sharing, IntPtr security,
            uint disposition, uint flags, IntPtr template);
        [DllImport("kernel32.dll", SetLastError=true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        static extern bool DeviceIoControl(SafeFileHandle handle, uint code, byte[] input, uint inputSize,
            [Out] byte[] output, uint outputSize, out uint returned, IntPtr overlapped);
        static SafeFileHandle Open(string root) { return CreateFileW(@"\\.\" + root.Substring(0,2), 0x80000000, 7, IntPtr.Zero, 3, 0, IntPtr.Zero); }
        static JournalPosition Query(SafeFileHandle handle) {
            var state = new JournalPosition();
            if (handle.IsInvalid) { state.Error = Marshal.GetLastWin32Error(); return state; }
            var buffer = new byte[80];
            uint returned;
            if (!DeviceIoControl(handle, QueryControl, null, 0, buffer, (uint)buffer.Length, out returned, IntPtr.Zero)) {
                state.Error = Marshal.GetLastWin32Error(); return state;
            }
            if (returned < 56) { state.Error = -1; return state; }
            state.Id=BitConverter.ToUInt64(buffer,0); state.First=BitConverter.ToInt64(buffer,8);
            state.Next=BitConverter.ToInt64(buffer,16); state.LowestValid=BitConverter.ToInt64(buffer,24);
            if (state.First < 0 || state.Next < state.First || state.LowestValid < 0) state.Error=-1;
            return state;
        }
        public static JournalPosition Query(string root) { using (var handle=Open(root)) { return Query(handle); } }
        public static JournalEvidence Read(string root, JournalPosition before, JournalPosition after, long firstTime, long lastTime) {
            var result = new JournalEvidence();
            result.JournalId=before.Id; result.StartUsn=before.Next; result.EndUsn=after.Next;
            if (before.Error != 0 || after.Error != 0 || before.Id != after.Id || after.Next < before.Next
                || after.First > before.Next || after.LowestValid > before.Next || firstTime > lastTime) {
                result.Failure="Journal unavailable, replaced, truncated or invalid scan time window"; return result;
            }
            using (var handle=Open(root)) {
                if (handle.IsInvalid) { result.Failure="Journal open error=" + Marshal.GetLastWin32Error(); return result; }
                var current=Query(handle);
                if (current.Error != 0 || current.Id != before.Id || current.First > before.Next || current.LowestValid > before.Next) {
                    result.Failure="Journal changed or was truncated before reconciliation"; return result;
                }
                var clock=Stopwatch.StartNew();
                var input=new byte[40];
                // BytesToWaitFor/Timeout=0、ReturnOnlyOnClose=0：到末尾立即返回，不等待卷继续产生变更。
                Array.Copy(BitConverter.GetBytes(UInt32.MaxValue),0,input,8,4);
                Array.Copy(BitConverter.GetBytes(before.Id),0,input,32,8);
                var buffer=new byte[1024*1024];
                long cursor=before.Next;
                long previousUsn=-1;
                while (cursor < after.Next) {
                    if (clock.Elapsed.TotalSeconds > 15 || result.BytesRead >= MaxBytes || result.RecordsRead >= MaxRecords) {
                        result.Failure="Journal diagnostic limit reached (15s/64MiB/200000 records)"; return result;
                    }
                    Array.Copy(BitConverter.GetBytes(cursor),0,input,0,8);
                    uint returned;
                    if (!DeviceIoControl(handle,ReadControl,input,(uint)input.Length,buffer,(uint)buffer.Length,out returned,IntPtr.Zero)) {
                        result.Failure="Journal read error=" + Marshal.GetLastWin32Error(); return result;
                    }
                    if (returned < 8 || returned > buffer.Length) { result.Failure="Invalid journal response length"; return result; }
                    result.BytesRead += returned;
                    long next=BitConverter.ToInt64(buffer,0);
                    int offset=8;
                    while (offset < returned) {
                        if (returned-offset < 8) { result.Failure="Truncated journal header"; return result; }
                        uint length=BitConverter.ToUInt32(buffer,offset);
                        ushort version=BitConverter.ToUInt16(buffer,offset+4);
                        if (length < 8 || (length & 7) != 0 || length > returned-offset) { result.Failure="Invalid journal record length"; return result; }
                        if (++result.RecordsRead > MaxRecords) { result.Failure="Journal record limit reached"; return result; }
                        if (version == 2) {
                            if (length < 60) { result.Failure="Short USN_RECORD_V2"; return result; }
                            long usn=BitConverter.ToInt64(buffer,offset+24), time=BitConverter.ToInt64(buffer,offset+32);
                            if (usn < cursor || usn <= previousUsn || usn >= next) { result.Failure="Non-increasing journal record USN"; return result; }
                            previousUsn=usn;
                            ushort nameBytes=BitConverter.ToUInt16(buffer,offset+56), nameOffset=BitConverter.ToUInt16(buffer,offset+58);
                            if (nameOffset < 60 || (nameOffset & 1) != 0 || (nameBytes & 1) != 0 || nameOffset+nameBytes > length) {
                                result.Failure="Invalid journal filename bounds"; return result;
                            }
                            if (usn >= before.Next && usn < after.Next) {
                                result.Observe(new Change { Reference=BitConverter.ToUInt64(buffer,offset+8), Parent=BitConverter.ToUInt64(buffer,offset+16),
                                    Usn=usn,FileTime=time,Reason=BitConverter.ToUInt32(buffer,offset+40),
                                    Name=NameEncoding.GetString(buffer,offset+nameOffset,nameBytes) }, time >= firstTime && time <= lastTime);
                            }
                        } else { result.UnsupportedRecords++; }
                        offset += (int)length;
                    }
                    if (next <= cursor) { result.Failure="Journal cursor did not advance"; return result; }
                    cursor=next;
                }
                // 不认识的版本不能被当成“没有变化”；整个证据集保持不完整。
                if (result.UnsupportedRecords != 0) { result.Failure="Unsupported USN record version"; return result; }
                result.Complete=true;
                return result;
            }
        }
    }
}
'@

function Read-ScanExport([string]$Path) {
    # 每条 JSON 解析后只留下 string/Int64，避免将数百万个 PSObject 常驻内存。
    $files = [Collections.Generic.Dictionary[string,long]]::new([StringComparer]::OrdinalIgnoreCase)
    $metadata = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $denied = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $summary = $null
    $ended = $false
    [long]$total = 0
    foreach ($line in [IO.File]::ReadLines($Path)) {
        if ($ended) { throw '结束行之后仍有数据 / Data follows export trailer.' }
        $doc = [Text.Json.JsonDocument]::Parse($line)
        try {
            $item = $doc.RootElement
            $kind = $item.GetProperty('type').GetString()
            if ($null -eq $summary -and $kind -ne 'summary') { throw '清单第一行必须是摘要 / Export must start with summary.' }
            switch ($kind) {
                'summary' {
                    if ($null -ne $summary) { throw '重复清单摘要 / Duplicate export summary.' }
                    $summary = $line | ConvertFrom-Json
                }
                'end' {
                    if ($item.GetProperty('files').GetInt64() -ne $files.Count -or $item.GetProperty('bytes').GetInt64() -ne $total) {
                        throw '清单结束行计数不符 / Export trailer totals disagree.'
                    }
                    $ended = $true
                }
                'file' {
                    $path = $item.GetProperty('path').GetString().Replace('/', '\')
                    $size = $item.GetProperty('size').GetInt64()
                    if ($size -lt 0 -or $files.ContainsKey($path)) { throw "非法或重复清单路径 / Invalid or duplicate export path: $path" }
                    $files.Add($path, $size)
                    $total += $size
                    # MFT 将记录 0–15 和 $Extend 后代文件放在卷根且归 Core；其他 Core 文件不能匹配此条件。
                    if ($item.GetProperty('category').GetString() -eq 'core' -and $path -match '^[A-Za-z]:\\\$[^\\]+$') {
                        [void]$metadata.Add($path)
                    }
                }
                'denied_dir' { [void]$denied.Add($item.GetProperty('path').GetString().Replace('/', '\').TrimEnd('\')) }
                default { throw '未知清单行类型 / Unknown export row type.' }
            }
        } finally { $doc.Dispose() }
    }
    if (-not $ended -or $null -eq $summary -or $summary.schema -ne 1 -or $summary.state -ne 'done') {
        throw '清单缺少完整摘要或结束行 / Missing completed summary or export trailer.'
    }
    if ($files.Count -ne $summary.files -or $total -ne $summary.bytes) {
        throw '清单计数与摘要不符 / Export totals disagree with summary.'
    }
    return @{ Files=$files; Metadata=$metadata; Denied=$denied; Summary=$summary }
}
function Read-Metadata([string]$Path, [bool]$Streams) {
    try { return [DiskMapAcceptance.Metadata]::Read($Path, $Streams) }
    catch {
        # 活动卷的单路径元数据查询失败不能中止整次对账，无法证明的差额保留在 other。
        $result = [DiskMapAcceptance.Info]::new()
        $result.Error = -1
        $result.StreamError = -1
        return $result
    }
}
function Read-HardLinks([string]$Path) {
    try { return [DiskMapAcceptance.Metadata]::HardLinks($Path) }
    catch { return @() }
}
function Find-DeniedAncestor([string]$Path, $Denied) {
    $position = $Path.LastIndexOf('\')
    while ($position -ge 2) {
        $parent = $Path.Substring(0, $position)
        if ($Denied.Contains($parent)) { return $parent }
        $position = $parent.LastIndexOf('\')
    }
    return $null
}
function Read-JournalPosition([string]$Root) {
    try { return [DiskMapAcceptance.Journal]::Query($Root) }
    catch {
        $position=[DiskMapAcceptance.JournalPosition]::new()
        $position.Error=-1
        return $position
    }
}
function Read-SummaryHead([string]$Path) {
    $reader=[IO.File]::OpenText($Path)
    try { return ($reader.ReadLine() | ConvertFrom-Json) } finally { $reader.Dispose() }
}
function Find-RaceEvidence([string]$Path, $Info, [string]$Presence) {
    if (-not $journal.Complete) { return $null }
    if ($Info.Error -eq 0 -and -not $journal.HasReference($Info.Reference)) { return $null }
    # 大小变化必须有同一文件 ID 的内容/流/压缩变更；仅 CLOSE、时间戳更新不算证据。
    if ($Presence -eq 'both' -and $Info.Error -eq 0) {
        return $journal.Match($Info.Reference,0,'',0x00220177,$false)
    }
    $parent=[IO.Path]::GetDirectoryName($Path)
    if (-not $parentIds.ContainsKey($parent)) {
        $parentInfo=Read-Metadata $parent $false
        $parentIds[$parent]= $(if ($parentInfo.Error -eq 0) { $parentInfo.Reference } else { [uint64]0 })
    }
    if ($parentIds[$parent] -eq 0) { return $null }
    [uint64]$reference=0
    if ($Info.Error -eq 0) { $reference=$Info.Reference }
    elseif ($Info.Error -ne 2 -and $Info.Error -ne 3) { return $null }
    # 名字缺失只认删除/旧名，名字新增只认创建/新名；不把一般写入或当前 NotFound 当作增删证明。
    [uint32]$mask= $(if ($Presence -eq 'generic') { 0x00002100 } else { 0x00001200 })
    return $journal.Match($reference,$parentIds[$parent],[IO.Path]::GetFileName($Path),$mask,$true)
}
function Show-ScanMetrics($Summary) {
    # 老版本导出没有阶段/峰值字段时继续显示已知数字，不把空值当成零。
    $timings=$null; $peak=$null
    if ($null -ne $Summary.PSObject.Properties['timings']) { $timings=$Summary.timings }
    if ($null -ne $Summary.PSObject.Properties['peak_rss_bytes']) { $peak=$Summary.peak_rss_bytes }
    if ($null -ne $timings) {
        Write-Host "扫描阶段秒数 / Scan stage seconds: $($Summary.method)"
        $timings | Select-Object io,parse,tree,classify,hardlinks,index | Format-Table | Out-Host
    }
    if ($null -ne $peak) { Write-Host ("峰值工作集 / Peak working set: {0} {1:N2} MiB" -f $Summary.method,($peak/1MB)) }
}

$previousCache = $env:DISKMAP_CACHE_DIR
$previousConfig = $env:DISKMAP_CONFIG_DIR
try {
    $env:DISKMAP_CACHE_DIR = Join-Path $OutputDir 'cache'
    $env:DISKMAP_CONFIG_DIR = Join-Path $OutputDir 'config'
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $administrator = [Security.Principal.WindowsPrincipal]::new($identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    Write-OutputJson 'environment.json' @{ utc=[DateTime]::UtcNow.ToString('o'); os=[Environment]::OSVersion.ToString();
        powershell=$PSVersionTable.PSVersion.ToString(); administrator=$administrator; scan_exe=$ScanExe; apparent=[bool]$Apparent;
        evidence='Live metadata is queried after scans; races require USN records in the scan time window. Unproved differences remain other.' }
    foreach ($root in $Roots) {
        $letter = $root.Substring(0,1)
        $snapshots = [Collections.Generic.List[object]]::new()
        $journalBefore=Read-JournalPosition $root
        foreach ($method in @('mft','generic')) {
            $snapshots.Add((Get-VolumeStamp $root))
            $name = "$letter-$method.jsonl"
            $exportPath = Join-Path $OutputDir $name
            $log = New-OutputWriter "$letter-$method.log"
            try {
                $scanArgs = @($root, '--method', $method, '--export', $exportPath)
                if ($Apparent) { $scanArgs += '--apparent' }
                & $ScanExe @scanArgs 2>&1 | ForEach-Object { $log.WriteLine([string]$_); Write-Host $_ }
                if ($LASTEXITCODE -ne 0) { throw "扫描失败；请检查日志 / Scan failed; inspect log: $letter-$method.log (exit=$LASTEXITCODE)" }
            } finally { $log.Dispose() }
            $snapshots.Add((Get-VolumeStamp $root))
        }
        $journalAfter=Read-JournalPosition $root
        Write-OutputJson "$letter-volume-stamps.json" $snapshots
        $reconciliationClock = [Diagnostics.Stopwatch]::StartNew()
        # 先取摘要中的精确扫描时间并读取 USN，避免大 JSONL 解析期间日志被截断。
        $mftHead=Read-SummaryHead (Join-Path $OutputDir "$letter-mft.jsonl")
        $genericHead=Read-SummaryHead (Join-Path $OutputDir "$letter-generic.jsonl")
        $journal=[DiskMapAcceptance.JournalEvidence]::new()
        if ($null -ne $mftHead.PSObject.Properties['scan_started_unix_ms'] -and
            $null -ne $genericHead.PSObject.Properties['scan_finished_unix_ms'] -and
            $null -ne $mftHead.scan_started_unix_ms -and $null -ne $genericHead.scan_finished_unix_ms) {
            try {
                $firstTime=[DateTimeOffset]::FromUnixTimeMilliseconds([long]$mftHead.scan_started_unix_ms).UtcDateTime.ToFileTimeUtc()
                $lastTime=[DateTimeOffset]::FromUnixTimeMilliseconds([long]$genericHead.scan_finished_unix_ms).UtcDateTime.ToFileTimeUtc()
                $journal=[DiskMapAcceptance.Journal]::Read($root,$journalBefore,$journalAfter,$firstTime,$lastTime)
            } catch { $journal.Failure='USN evidence could not be read: ' + $_.Exception.Message }
        } else { $journal.Failure='Legacy export has no exact scan timestamps; races cannot be proved.' }
        $journalSummary=[ordered]@{ complete=$journal.Complete; failure=$journal.Failure;
            before=$journalBefore; after=$journalAfter; bytes_read=$journal.BytesRead; records_read=$journal.RecordsRead;
            unsupported_records=$journal.UnsupportedRecords; scan_started_unix_ms=$null; scan_finished_unix_ms=$null }
        if ($null -ne $mftHead.PSObject.Properties['scan_started_unix_ms']) { $journalSummary.scan_started_unix_ms=$mftHead.scan_started_unix_ms }
        if ($null -ne $genericHead.PSObject.Properties['scan_finished_unix_ms']) { $journalSummary.scan_finished_unix_ms=$genericHead.scan_finished_unix_ms }
        Write-OutputJson "$letter-usn-window.json" $journalSummary
        if (-not $journal.Complete) { Write-Host "竞态证据不可用 / Race evidence unavailable: $($journal.Failure)" }
        $mft = Read-ScanExport (Join-Path $OutputDir "$letter-mft.jsonl")
        $generic = Read-ScanExport (Join-Path $OutputDir "$letter-generic.jsonl")
        if ($mft.Summary.method -ne 'mft' -or $generic.Summary.method -ne 'generic' -or
            $mft.Summary.apparent -ne $generic.Summary.apparent -or $mft.Summary.root -ne $generic.Summary.root) {
            throw '清单扫描方式/根目录/口径不符 / Export methods, roots or size modes disagree.'
        }
        $totals = [ordered]@{}
        foreach ($kind in @('hardlinks','ads','metadata','permission','compressed_sparse','race','other')) {
            $totals[$kind] = @{ paths=0L; bytes=0L; file_delta=0L }
        }
        $artifactTotals = @{ bytes=0L; file_delta=0L; rows=0L }
        $top = [Collections.Generic.List[object]]::new()
        $identities = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::OrdinalIgnoreCase)
        $canonical = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::Ordinal)
        $parentIds = [Collections.Generic.Dictionary[string,uint64]]::new([StringComparer]::OrdinalIgnoreCase)
        $diffWriter = New-OutputWriter "$letter-differences.jsonl"
        # 每项文件差额可拆成已证实的 ADS 部分和 other；每字节只登记一次。
        function Add-Difference([string]$Kind, [string]$Path, [long]$Delta, [long]$FileDelta, [string]$Evidence, [long]$MftBytes, [long]$GenericBytes) {
            if ($Path.StartsWith($OutputDir.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
                $artifactTotals.bytes += $Delta
                $artifactTotals.file_delta += $FileDelta
                $artifactTotals.rows += 1
                $Evidence = 'Acceptance-generated temporary output; ' + $Evidence
            }
            $totals[$Kind].paths += 1
            $totals[$Kind].bytes += $Delta
            $totals[$Kind].file_delta += $FileDelta
            $row = [pscustomobject]@{ kind=$Kind; path=$Path; delta_bytes=$Delta; file_delta=$FileDelta;
                mft_bytes=$MftBytes; generic_bytes=$GenericBytes; evidence=$Evidence }
            $diffWriter.WriteLine(($row | ConvertTo-Json -Compress))
            if ($Kind -eq 'other') {
                $top.Add($row)
                if ($top.Count -gt 20) {
                    $smallest=0
                    for ($i=1; $i -lt $top.Count; $i++) {
                        if ([Math]::Abs($top[$i].delta_bytes) -lt [Math]::Abs($top[$smallest].delta_bytes)) { $smallest=$i }
                    }
                    $top.RemoveAt($smallest)
                }
            }
        }
        try {
            foreach ($entry in $mft.Files.GetEnumerator()) {
                $path=$entry.Key; [long]$m=$entry.Value; [long]$g=0
                if ($generic.Files.TryGetValue($path, [ref]$g)) {
                    [long]$delta=$m-$g
                    if ($delta -eq 0) { continue }
                    $info=Read-Metadata $path $true
                    if ($info.Error -eq 0) { $identities[$path]=$info.Id }
                    [long]$ads=0; [long]$default=0
                    if ($info.StreamsValid) {
                        if ($Apparent) { $ads=$info.AdsSize; $default=$info.DefaultSize }
                        else { $ads=$info.AdsAllocated; $default=$info.DefaultAllocated }
                    }
                    $adsCounted=$false
                    # 已能独立证实的 ADS 不因同文件出现一般写入记录而全部变成“竞态”。
                    if ($ads -gt 0 -and $default -eq $g -and $delta -ge $ads) {
                        Add-Difference 'ads' $path $ads 0 "FileStreamInfo named DATA sum=$ads; default stream=$default" $m $g
                        $delta -= $ads
                        $adsCounted=$true
                    }
                    if ($delta -eq 0) { continue }
                    $race=Find-RaceEvidence $path $info 'both'
                    if ($null -ne $race) {
                        Add-Difference 'race' $path $delta 0 $race $m $g
                        continue
                    }
                    # 句柄默认流分配 + ADS 与 MFT 完全吻合时，把 ADS 与目录 API 的口径差拆开。
                    if (-not $Apparent -and $info.StandardValid -and $info.StreamsValid -and
                        $info.StandardSize -eq $info.DefaultSize -and ($info.StandardAllocated + $ads) -eq $m) {
                        if ($ads -gt 0 -and -not $adsCounted) {
                            Add-Difference 'ads' $path $ads 0 "FileStreamInfo named DATA sum=$ads; FileStandardInfo default allocation=$($info.StandardAllocated)" $m $g
                            $delta -= $ads
                        }
                        if ($delta -ne 0) {
                            Add-Difference 'compressed_sparse' $path $delta 0 "FileStandardInfo allocation=$($info.StandardAllocated) plus ADS=$ads equals MFT=$m; generic=$g; attributes=0x$($info.Attributes.ToString('x8'))" $m $g
                        }
                        continue
                    }
                    if ($delta -ne 0) {
                        Add-Difference 'other' $path $delta 0 "unproved residual; metadata_error=$($info.Error); stream_error=$($info.StreamError); current_default=$default; current_ads=$ads" $m $g
                    }
                } else {
                    $denied=Find-DeniedAncestor $path $generic.Denied
                    if ($mft.Metadata.Contains($path)) {
                        Add-Difference 'metadata' $path $m 1 'MFT export: Core file with $ name at volume root (metadata placement rule)' $m 0
                    } elseif ($null -ne $denied) {
                        Add-Difference 'permission' $path $m 1 "Generic index recorded DENIED at $denied" $m 0
                    } else {
                        $info=Read-Metadata $path $false
                        $race=Find-RaceEvidence $path $info 'mft'
                        if ($null -ne $race) { Add-Difference 'race' $path $m 1 $race $m 0 }
                        else { Add-Difference 'other' $path $m 1 "Only in MFT; no metadata, denied-parent or USN deletion evidence; metadata_error=$($info.Error)" $m 0 }
                    }
                }
            }
            foreach ($entry in $generic.Files.GetEnumerator()) {
                $path=$entry.Key; [long]$g=$entry.Value
                if ($mft.Files.ContainsKey($path)) { continue }
                $info=Read-Metadata $path $false
                $race=Find-RaceEvidence $path $info 'generic'
                if ($null -ne $race) {
                    Add-Difference 'race' $path (-$g) -1 $race 0 $g
                    continue
                }
                $match=$null
                if ($info.Error -eq 0 -and $info.Links -gt 1) {
                    if ($canonical.ContainsKey($info.Id)) { $match=$canonical[$info.Id] }
                    else {
                        foreach ($link in (Read-HardLinks $path)) {
                            $candidate=$root.TrimEnd('\') + $link
                            if (-not $mft.Files.ContainsKey($candidate)) { continue }
                            if (-not $identities.ContainsKey($candidate)) {
                                $other=Read-Metadata $candidate $false
                                if ($other.Error -eq 0) { $identities[$candidate]=$other.Id }
                            }
                            if ($identities.ContainsKey($candidate) -and $identities[$candidate] -eq $info.Id) {
                                $match=$candidate; $canonical[$info.Id]=$candidate; break
                            }
                        }
                    }
                }
                if ($null -ne $match) {
                    Add-Difference 'hardlinks' $path (-$g) -1 "Same volume/file ID $($info.Id); counted MFT name=$match" 0 $g
                } else {
                    Add-Difference 'other' $path (-$g) -1 "Only in generic; no matching counted hardlink identity; metadata_error=$($info.Error)" 0 $g
                }
            }
        } finally { $diffWriter.Dispose() }
        [long]$expected=[long]$mft.Summary.bytes-[long]$generic.Summary.bytes
        [long]$expectedFiles=[long]$mft.Summary.files-[long]$generic.Summary.files
        [long]$explained=0; [long]$explainedFiles=0
        foreach ($category in $totals.Values) { $explained += $category.bytes; $explainedFiles += $category.file_delta }
        if ($expected -ne $explained -or $expectedFiles -ne $explainedFiles) { throw '差额未对平 / Reconciliation does not balance.' }
        $topSorted=@($top | Sort-Object { [Math]::Abs($_.delta_bytes) } -Descending)
        $reconciliationClock.Stop()
        $result=[ordered]@{ root=$root; apparent=[bool]$Apparent; mft=$mft.Summary; generic=$generic.Summary;
            reconciliation_elapsed_seconds=$reconciliationClock.Elapsed.TotalSeconds;
            difference_bytes=$expected; difference_files=$expectedFiles; categories=$totals; other_top20=$topSorted;
            usn_window=$journalSummary;
            acceptance_artifact_difference=$artifactTotals;
            difference_excluding_acceptance_artifacts=@{ bytes=$expected-$artifactTotals.bytes; files=$expectedFiles-$artifactTotals.file_delta };
            note='Positive = MFT minus generic. Proven handle allocation differences and USN changes have separate categories; other remains unexplained.' }
        Write-OutputJson "$letter-reconciliation.json" $result
        Write-Host "卷 / Volume: $root"
        @($mft.Summary, $generic.Summary) | Select-Object method,files,dirs,bytes,elapsed_seconds,records,anomalies | Format-Table | Out-Host
        Show-ScanMetrics $mft.Summary
        Show-ScanMetrics $generic.Summary
        $totals.GetEnumerator() | ForEach-Object { [pscustomobject]@{ category=$_.Key; paths=$_.Value.paths; delta_bytes=$_.Value.bytes; file_delta=$_.Value.file_delta } } | Format-Table | Out-Host
        Write-Host '其他：差额最大的 20 个路径 / Other: top 20 absolute differences'
        $topSorted | Format-Table path,delta_bytes,mft_bytes,generic_bytes -AutoSize | Out-Host
        Write-Host "对账耗时 / Reconciliation seconds: $($reconciliationClock.Elapsed.TotalSeconds)"
        Write-Host "本次验收输出造成的差额 / Acceptance artifact delta: bytes=$($artifactTotals.bytes) files=$($artifactTotals.file_delta)"
        # 下一卷前释放两张全路径表；不同时保留多个卷的数据。
        $mft=$null; $generic=$null; $identities=$null; $canonical=$null; $parentIds=$null; $journal=$null
        [GC]::Collect()
    }
} finally {
    $env:DISKMAP_CACHE_DIR=$previousCache
    $env:DISKMAP_CONFIG_DIR=$previousConfig
    Write-Host "验收输出保留在 / Acceptance output retained at: $OutputDir"
}
