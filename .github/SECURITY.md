# Security policy

Only the latest release receives security fixes. Before a stable release, reports
against main are welcome. There is no guaranteed response or resolution time.

Report vulnerabilities privately using [Report a vulnerability](https://github.com/tomblanchard312/Ninimma-FileManager/security/advisories/new).
Do not put secrets, private files, or exploit details in public issues. Include the
version, platform, impact, and minimal reproduction using made-up data.

Release installers and checksum files built by the updated release workflows receive
GitHub artifact attestations. Verify a downloaded file with:

```sh
gh attestation verify DOWNLOAD -R tomblanchard312/Ninimma-FileManager
```

Attestations establish build provenance; they do not replace Windows Authenticode or
Apple signing and notarization, or prove that software has no vulnerabilities.
Earlier releases are not retroactively attested. Do not trust an asset solely
because its filename or checksum looks right.
