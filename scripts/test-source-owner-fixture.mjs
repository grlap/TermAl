// Test-only glue to the producer-owned fixture library supplied by the runner.
// This file defines no source cases, expected identities or Git judgment.
import { isAbsolute } from "node:path";
import { pathToFileURL } from "node:url";

const [modulePath, parent, id] = process.argv.slice(2);
if (!modulePath || !parent || !id || !isAbsolute(modulePath) || !isAbsolute(parent)) {
  throw new Error("expected absolute owner fixture module, absolute parent and case id");
}
const { sourceCases, createSourceFixture } = await import(pathToFileURL(modulePath).href);
if (id === "--list") {
  process.stdout.write(JSON.stringify(sourceCases.map(({ id, skipOn }) => ({
    id, skipped: skipOn?.includes(process.platform) ?? false,
  }))));
} else {
  const scenario = sourceCases.find((entry) => entry.id === id);
  if (!scenario || scenario.skipOn?.includes(process.platform)) {
    throw new Error("unknown or unsupported owner fixture case");
  }
  const fixture = createSourceFixture(parent, id);
  process.stdout.write(JSON.stringify({
    root: fixture.root, status: fixture.status, sourceId: fixture.sourceId,
  }));
}
