// Disposable integration witness for the shipped registration adapter against
// the Rust test's actual loopback router. Owns no host or registration policy.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { registerTestRun, validateRunRegistration } from "./test-run-registration.mjs";

const [base, session, directory] = process.argv.slice(2);
const request = JSON.parse(readFileSync(join(directory, "request.json"), "utf8"));
const env = { TERMAL_BASE_URL: base, TERMAL_SESSION_ID: session };
const registration = await registerTestRun(request, directory, env);
assert.equal(registration.state, "registered");
await validateRunRegistration({ ...request, registration }, env, directory);
console.log("REGISTRATION_VIEW_CONFIRMED");
