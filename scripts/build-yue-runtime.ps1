param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OutputDirectory,
    [ValidateSet('auto', 'cuda', 'vulkan', 'all')]
    [string]$RuntimeBackend = 'auto',
    [ValidateSet('universal', 'native', 'sm_89')]
    [string]$CudaArchitecture = 'universal'
)

# Cmdlet failures stop the build; native tools report progress on stderr, so
# for them the exit code is the verdict.
$PSDefaultParameterValues['*:ErrorAction'] = 'Stop'
$ErrorActionPreference = 'Continue'

$repoRoot = Split-Path -Parent $PSScriptRoot
$engineSource = Get-Content -Raw (Join-Path $repoRoot 'engines\yue2-cpp-source.json') | ConvertFrom-Json
# The ggml build tree is deep; a short root keeps it under MAX_PATH.
$engineBuildRoot = if ($env:YUE_ENGINE_BUILD_ROOT) { $env:YUE_ENGINE_BUILD_ROOT } else { $env:TEMP }
$engineWorktree = Join-Path $engineBuildRoot "yue2-$($engineSource.commit.Substring(0, 8))"
$shippedTargets = @('yue-server')

function Test-CudaToolchain {
    $nvcc = Get-Command nvcc -ErrorAction SilentlyContinue
    return [bool]$nvcc
}

function Assert-VulkanSdk {
    $sdk = $env:VULKAN_SDK
    $glslc = Get-Command glslc -ErrorAction SilentlyContinue
    if ([string]::IsNullOrWhiteSpace($sdk) -or -not (Test-Path (Join-Path $sdk 'Include\vulkan\vulkan.h')) -or -not $glslc) {
        throw 'The Vulkan build requires a Vulkan SDK with headers, vulkan-1.lib and glslc.'
    }
}

function Resolve-RuntimeBackend {
    switch ($RuntimeBackend) {
        'auto' {
            if (Test-CudaToolchain) { return 'cuda' }
            if ($env:VULKAN_SDK) { Assert-VulkanSdk; return 'vulkan' }
            throw 'No native toolchain found. Install the CUDA Toolkit or a Vulkan SDK.'
        }
        'cuda' { if (-not (Test-CudaToolchain)) { throw 'The CUDA build requires nvcc.' } }
        'vulkan' { Assert-VulkanSdk }
        'all' {
            if (-not (Test-CudaToolchain)) { throw 'The all-backends build requires nvcc.' }
            Assert-VulkanSdk
        }
    }
    return $RuntimeBackend
}

function Get-VcVars64 {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { throw 'vswhere.exe was not found; install Visual Studio C++ Build Tools.' }
    $installationPath = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($installationPath)) {
        throw 'No Visual Studio C++ build installation was found.'
    }
    $vcvars = Join-Path $installationPath.Trim() 'VC\Auxiliary\Build\vcvars64.bat'
    if (-not (Test-Path $vcvars)) { throw "vcvars64.bat is missing: $vcvars" }
    return $vcvars
}

function Sync-PinnedSource {
    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) { throw 'CMake is required on PATH.' }
    if (-not (Test-Path (Join-Path $engineWorktree '.git'))) {
        git clone --recurse-submodules $engineSource.repository $engineWorktree
        if ($LASTEXITCODE -ne 0) { throw 'Could not clone yue2.cpp.' }
    }
    git -C $engineWorktree fetch origin $engineSource.commit
    if ($LASTEXITCODE -ne 0) { throw "Could not fetch yue2.cpp commit $($engineSource.commit)." }
    git -C $engineWorktree checkout --detach $engineSource.commit
    if ($LASTEXITCODE -ne 0) { throw "Could not check out yue2.cpp commit $($engineSource.commit)." }
    git -C $engineWorktree submodule update --init --recursive
    if ($LASTEXITCODE -ne 0) { throw 'Could not initialise yue2.cpp submodules.' }
}

function Invoke-CMakeBuild([string]$backend) {
    # GGML_NATIVE=OFF plus an explicit architecture list: a binary that runs on
    # other people's CPUs and on Turing through Blackwell, with PTX for JIT.
    $cudaArch = switch ($CudaArchitecture) {
        'universal' { '-DGGML_NATIVE=OFF "-DCMAKE_CUDA_ARCHITECTURES=75-virtual;80-virtual;86-real;89-real;90-virtual;120a-real;120-virtual"' }
        'native' { '-DCMAKE_CUDA_ARCHITECTURES=native' }
        'sm_89' { '-DCMAKE_CUDA_ARCHITECTURES=89' }
    }
    $backendFlags = switch ($backend) {
        'cuda' { "-DGGML_CUDA=ON $cudaArch" }
        'vulkan' { '-DGGML_NATIVE=OFF -DGGML_VULKAN=ON' }
        'all' { "-DGGML_BACKEND_DL=ON -DGGML_CPU_ALL_VARIANTS=ON -DGGML_VULKAN=ON -DGGML_CUDA=ON $cudaArch" }
    }
    $ccache = if (Get-Command ccache -ErrorAction SilentlyContinue) { '-DGGML_CCACHE=ON' } else { '-DGGML_CCACHE=OFF' }
    # Program databases beside the binaries turn a crash offset into a line.
    $symbols = '-DCMAKE_MSVC_DEBUG_INFORMATION_FORMAT=ProgramDatabase -DCMAKE_EXE_LINKER_FLAGS=/DEBUG -DCMAKE_SHARED_LINKER_FLAGS=/DEBUG'
    $buildDirectoryName = "build-$backend-$CudaArchitecture"
    $targets = ($shippedTargets | ForEach-Object { "--target $_" }) -join ' '
    $parallelism = [Math]::Max(1, [Environment]::ProcessorCount)
    $vcvars = Get-VcVars64
    # Ninja drives nvcc and cl directly, so the build does not depend on the
    # CUDA MSBuild integration being installed into this Visual Studio.
    if (-not (Get-Command ninja -ErrorAction SilentlyContinue)) { throw 'Ninja is required on PATH.' }
    $command = "call `"$vcvars`" >nul && cmake -S . -B `"$buildDirectoryName`" -G Ninja -DCMAKE_BUILD_TYPE=Release $ccache $symbols $backendFlags && cmake --build `"$buildDirectoryName`" $targets --parallel $parallelism"
    Push-Location $engineWorktree
    try { & cmd.exe /d /s /c $command | Out-Host } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw "yue2.cpp $backend build failed." }
    return $buildDirectoryName
}

Sync-PinnedSource
$backend = Resolve-RuntimeBackend
$buildDirectoryName = Invoke-CMakeBuild $backend
$binDirectory = @(
    (Join-Path $engineWorktree "$buildDirectoryName\Release"),
    (Join-Path $engineWorktree "$buildDirectoryName\bin\Release"),
    (Join-Path $engineWorktree $buildDirectoryName)
) | Where-Object { Test-Path (Join-Path $_ 'yue-server.exe') } | Select-Object -First 1
if (-not $binDirectory) { throw 'The yue2.cpp build completed without yue-server.exe.' }

$resolvedOutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
if ($resolvedOutputDirectory -eq [System.IO.Path]::GetPathRoot($resolvedOutputDirectory)) {
    throw 'OutputDirectory must be a specific child directory, not a drive root.'
}
New-Item -ItemType Directory -Force -Path $resolvedOutputDirectory | Out-Null
foreach ($target in $shippedTargets) {
    Copy-Item (Join-Path $binDirectory "$target.exe") $resolvedOutputDirectory -Force
    $pdb = Join-Path $binDirectory "$target.pdb"
    if (Test-Path $pdb) { Copy-Item $pdb $resolvedOutputDirectory -Force }
}
Get-ChildItem -Path $binDirectory -Filter '*.dll' -File | Copy-Item -Destination $resolvedOutputDirectory -Force

# What was staged, so a release can tell whether it must rebuild.
$stamp = [pscustomobject]@{
    commit = $engineSource.commit
    backend = $backend
    cuda_architecture = $CudaArchitecture
    runtime = 'yue-server.exe'
}
[System.IO.File]::WriteAllText((Join-Path $resolvedOutputDirectory 'runtime.json'), ($stamp | ConvertTo-Json), (New-Object System.Text.UTF8Encoding($false)))
$stamp | ConvertTo-Json -Compress
