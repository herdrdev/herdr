$ErrorActionPreference = 'Stop'
function Assert-PretorianNativeMachine([UInt16]$Maquina) {
    if ($Maquina -ne 0x8664) {
        throw 'Esta plataforma ainda não possui instalador aprovado.'
    }
}
if (-not ('PretorianNativeMachine' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class PretorianNativeMachine {
    [DllImport("kernel32.dll")]
    public static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWow64Process2(IntPtr process, out ushort processMachine, out ushort nativeMachine);
}
'@
}
$MaquinaProcesso = [UInt16]0
$MaquinaNativa = [UInt16]0
try {
    $Identificada = [PretorianNativeMachine]::IsWow64Process2([PretorianNativeMachine]::GetCurrentProcess(), [ref]$MaquinaProcesso, [ref]$MaquinaNativa)
} catch {
    throw 'Não foi possível confirmar a arquitetura nativa desta plataforma.'
}
if (-not $Identificada) { throw 'Não foi possível confirmar a arquitetura nativa desta plataforma.' }
Assert-PretorianNativeMachine $MaquinaNativa
$Temporario = Join-Path ([IO.Path]::GetTempPath()) ('pretorian-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Temporario | Out-Null
try {
    $Pacote = Join-Path $Temporario 'gh.zip'
    Invoke-WebRequest 'https://github.com/cli/cli/releases/download/v2.96.0/gh_2.96.0_windows_amd64.zip' -OutFile $Pacote
    $Soma = (Get-FileHash -Algorithm SHA256 $Pacote).Hash.ToLowerInvariant()
    if ($Soma -cne 'c2d6acc935cd2f00e2144d7e036d5cd82e6b6bd5594e8c75aa75ef2a4ed6aac3') { throw 'Integridade do GitHub CLI não confirmada.' }
    Expand-Archive -LiteralPath $Pacote -DestinationPath (Join-Path $Temporario 'gh')
    $Binarios = @(Get-ChildItem -Path (Join-Path $Temporario 'gh') -Filter gh.exe -File -Recurse)
    if ($Binarios.Count -ne 1) { throw 'GitHub CLI não encontrado no pacote.' }
    $Gh = $Binarios[0].FullName
    $env:GH_CONFIG_DIR = Join-Path $env:LOCALAPPDATA 'Pretorian\github'
    New-Item -ItemType Directory -Path $env:GH_CONFIG_DIR -Force | Out-Null
    Remove-Item Env:GH_TOKEN,Env:GITHUB_TOKEN -ErrorAction SilentlyContinue
    & $Gh auth status --hostname github.com 2>$null
    if ($LASTEXITCODE -ne 0) {
        & $Gh auth login --hostname github.com --git-protocol https --web
        if ($LASTEXITCODE -ne 0) { throw 'Autenticação no GitHub não concluída.' }
    }
    $env:PATH = $Binarios[0].DirectoryName + [IO.Path]::PathSeparator + $env:PATH
    $Nome = 'pretorian-instalar-windows-x64.exe'
    $Release = & $Gh api repos/lamarck-distribution-center/pretorian-cli/releases/latest | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Versão do instalador indisponível.' }
    if ($Release.tag_name -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') { throw 'Versão de distribuição inválida.' }
    $Ativos = @($Release.assets | Where-Object { $_.name -ceq $Nome })
    if ($Ativos.Count -ne 1 -or $Ativos[0].digest -notmatch '^sha256:[a-f0-9]{64}$') { throw 'Instalador não possui hash verificável.' }
    & $Gh release download $Release.tag_name --repo lamarck-distribution-center/pretorian-cli --pattern $Nome --dir $Temporario
    if ($LASTEXITCODE -ne 0) { throw 'Não foi possível baixar o instalador.' }
    $Instalador = Join-Path $Temporario $Nome
    $Digest = 'sha256:' + (Get-FileHash -Algorithm SHA256 $Instalador).Hash.ToLowerInvariant()
    if ($Digest -cne $Ativos[0].digest) { throw 'Integridade do instalador não confirmada.' }
    & $Instalador @args
    if ($LASTEXITCODE -ne 0) { throw 'A instalação não foi concluída.' }
} finally {
    Remove-Item -Recurse -Force -LiteralPath $Temporario -ErrorAction SilentlyContinue
}
