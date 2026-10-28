import { existsSync } from 'node:fs';
import { join } from 'node:path';

/** VS Code's platform-specific VSIX targets (`vsce package --target`); one engine binary is bundled per target. */
export const SUPPORTED_TARGETS = ['win32-x64', 'win32-arm64', 'darwin-x64', 'darwin-arm64', 'linux-x64', 'linux-arm64'] as const;

export interface BinaryLookup {
  extensionPath: string;
  /** `goku.enginePath` (machine scope): an engine build to use instead of the bundled one. */
  override?: string;
  platform?: NodeJS.Platform;
  arch?: string;
  exists?: (path: string) => boolean;
}

export function bundledBinaryPath(extensionPath: string, platform: NodeJS.Platform, arch: string): string {
  const exe = platform === 'win32' ? 'goku-engine-server.exe' : 'goku-engine-server';
  return join(extensionPath, 'bin', `${platform}-${arch}`, exe);
}

/** The engine to spawn, or an error worded for the user. */
export function resolveBinary(o: BinaryLookup): string {
  const exists = o.exists ?? existsSync;
  const override = o.override?.trim();
  if (override) {
    if (!exists(override)) throw new Error(`goku.enginePath points to “${override}”, which doesn't exist.`);
    return override;
  }
  const platform = o.platform ?? process.platform;
  const arch = o.arch ?? process.arch;
  const path = bundledBinaryPath(o.extensionPath, platform, arch);
  if (exists(path)) return path;
  const target = `${platform}-${arch}`;
  throw new Error(
    (SUPPORTED_TARGETS as readonly string[]).includes(target)
      ? `The Goku engine for ${target} is missing from this installation. Reinstall the extension.`
      : `Goku has no engine for ${target}. Supported: ${SUPPORTED_TARGETS.join(', ')}.`,
  );
}
