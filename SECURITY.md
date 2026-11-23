# Security

Rhue is meant to run scripts you don't trust, so anything that lets one get past that is a security bug:

- reaching the filesystem, network, processes or anything else the host didn't hand it
- getting around the instruction, memory or recursion limits, or an interrupt
- one environment changing what another one sees, like the libraries they share once `Engine::freeze_libraries` is on
- panicking the host, or hanging it in a way none of the limits can stop
- loaded bytecode that passes the verifier and then does something it shouldn't

Report it privately through GitHub's [vulnerability reporting](https://github.com/rhuelang/rhue/security/advisories/new) rather than an issue. Include a script (or chunk) that shows it, and which version you found it on.

I'll get back to you within a week, and fixes land in the latest release. Only the newest major version gets security fixes.
