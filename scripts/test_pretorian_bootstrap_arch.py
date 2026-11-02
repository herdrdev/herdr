import os
import shutil
import subprocess
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / 'distribution/pretorian/instalar.ps1'
POWERSHELL = shutil.which('powershell.exe')

HARNESS = r'''
$ErrorActionPreference = 'Stop'
function global:New-Item { throw 'EFFECT_CREATE_BLOCKED' }
function global:Invoke-WebRequest { throw 'EFFECT_DOWNLOAD_BLOCKED' }
if ($env:PRETORIAN_TEST_FAKE -eq '1') {
    Add-Type -TypeDefinition @'
using System;
public static class PretorianNativeMachine {
    public static IntPtr GetCurrentProcess() { return IntPtr.Zero; }
    public static bool IsWow64Process2(IntPtr process, out ushort processMachine, out ushort nativeMachine) {
        processMachine = 0x8664;
        nativeMachine = UInt16.Parse(Environment.GetEnvironmentVariable("PRETORIAN_TEST_MACHINE"));
        return Environment.GetEnvironmentVariable("PRETORIAN_TEST_QUERY") == "true";
    }
}
'@
}
$env:PROCESSOR_ARCHITECTURE = 'AMD64'
$result = 'SCRIPT_RETURNED'
try { & $env:PRETORIAN_TEST_BOOTSTRAP } catch { $result = $_.Exception.Message }
if ($env:PRETORIAN_TEST_FAKE -ne '1') {
    $processMachine = [UInt16]0
    $nativeMachine = [UInt16]0
    $ok = [PretorianNativeMachine]::IsWow64Process2([PretorianNativeMachine]::GetCurrentProcess(), [ref]$processMachine, [ref]$nativeMachine)
    if (-not $ok) { throw 'A consulta real da arquitetura falhou' }
    Write-Output ('NATIVE:' + $nativeMachine)
}
Write-Output ('RESULT:' + $result)
'''


@unittest.skipUnless(os.name == 'nt', 'Exige Windows nativo')
class BootstrapArchitecture(unittest.TestCase):
    def run_bootstrap(self, native=None, query=True):
        self.assertIsNotNone(POWERSHELL, 'PowerShell nativo obrigatório no Windows')
        env = os.environ.copy()
        env['PRETORIAN_TEST_BOOTSTRAP'] = str(SCRIPT)
        env['PRETORIAN_TEST_FAKE'] = '1' if native is not None else '0'
        env['PRETORIAN_TEST_MACHINE'] = str(native or 0)
        env['PRETORIAN_TEST_QUERY'] = 'true' if query else 'false'
        result = subprocess.run([POWERSHELL, '-NoProfile', '-NonInteractive', '-Command', HARNESS],
                                env=env, check=True, capture_output=True, text=True)
        lines = result.stdout.splitlines()
        response = next(line.removeprefix('RESULT:') for line in lines if line.startswith('RESULT:'))
        machine = next((int(line.removeprefix('NATIVE:')) for line in lines if line.startswith('NATIVE:')), None)
        return response, machine

    def test_full_script_rejects_unsupported_native_architecture_before_effects(self):
        for native, query in ((0xaa64, True), (0x014c, True), (0x8664, False)):
            with self.subTest(native=native, query=query):
                result, _ = self.run_bootstrap(native, query)
                self.assertNotIn('EFFECT_', result)
                self.assertNotEqual(result, 'SCRIPT_RETURNED')
                if query:
                    self.assertIn('plataforma', result)
                else:
                    self.assertIn('arquitetura nativa', result)

    def test_full_script_x64_reaches_blocked_first_effect(self):
        result, _ = self.run_bootstrap(0x8664)
        self.assertIn('EFFECT_CREATE_BLOCKED', result)
        self.assertNotIn('EFFECT_DOWNLOAD_BLOCKED', result)

    def test_actual_windows_native_architecture_controls_first_effect(self):
        result, native = self.run_bootstrap()
        self.assertIsNotNone(native)
        if native == 0x8664:
            self.assertIn('EFFECT_CREATE_BLOCKED', result)
        else:
            self.assertIn('plataforma', result)
            self.assertNotIn('EFFECT_', result)
        self.assertNotIn('EFFECT_DOWNLOAD_BLOCKED', result)


if __name__ == '__main__':
    unittest.main()
