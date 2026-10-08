Real TRX results captured from `dotnet test --logger "trx;LogFileName=results.trx"
--results-directory .termal-results` on a scratch xUnit v2 project (net10.0,
xunit 2.9.3, xunit.runner.visualstudio 3.1.4, Microsoft.NET.Test.Sdk 17.14.1).
Machine, user and path fields are replaced by neutral values; structure,
outcomes and counters are as the logger wrote them.

- `pass.trx`: three passing tests, exit 0.
- `fail.trx`: two passing and one failing test, exit 1.
- `zero.trx`: a project with no tests, exit 0.
- `mixed.trx`: two passing tests and one skipped (`[Fact(Skip = ...)]`), exit 0;
  the skipped test is in `total` only, not in `executed` or `notExecuted`.
