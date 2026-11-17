import { execFileSync } from "node:child_process";
import { chmodSync, cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const outputDirectory = process.argv[2];
if (!outputDirectory || process.argv.length !== 3) {
	throw new Error("Usage: node crates/pi-evals/docker/install-runtime.mjs <output-directory>");
}

const targetDir = process.env.CARGO_TARGET_DIR
	? resolve(process.env.CARGO_TARGET_DIR)
	: join(repositoryRoot, "target");

execFileSync(
	"cargo",
	["build", "--locked", "--release", "-p", "pi-evals", "--bin", "pi-evals"],
	{
		cwd: repositoryRoot,
		stdio: "inherit",
		env: { ...process.env, CARGO_TARGET_DIR: targetDir },
	},
);

const runnerBinary = join(targetDir, "release", "pi-evals");
if (!existsSync(runnerBinary)) {
	throw new Error(`Compiled pi-evals runner binary not found: ${runnerBinary}`);
}

const packageSource = join(repositoryRoot, "crates/pi-coding-agent/package");
if (!existsSync(packageSource)) {
	throw new Error(`Coding-agent package assets not found: ${packageSource}`);
}

const rootBin = join(outputDirectory, "root/bin");
mkdirSync(rootBin, { recursive: true });
cpSync(runnerBinary, join(rootBin, "pi-evals"));
chmodSync(join(rootBin, "pi-evals"), 0o700);

const installPackage = join(outputDirectory, "install/package");
if (existsSync(installPackage)) {
	rmSync(installPackage, { force: true, recursive: true });
}
cpSync(packageSource, installPackage, { recursive: true });
