# Releasing

Versions come from Git: release tags are `v<major>.<minor>`, and builds in
between are `<major>.<minor>.r<commits>.g<hash>` (`0.0.r…` before the first
tag). The version code is `major << 16 | minor << 8 | commits`.

## One-time setup

1. Create the release key and keep it (and its passphrases) outside the
   repository. The module's supervisor pins the app's signing certificate,
   so every release must use the same key: changing it means users have to
   uninstall the app before updating.

   ```bash
   keytool -genkeypair -keystore uvcamera-release.jks -storetype PKCS12 \
       -alias uvcamera -keyalg EC -groupname secp384r1 -validity 10000 \
       -dname CN=UVCamera
   ```

2. Commit the certificate's SHA-256 as `release-signing-certificate.sha256`.
   `scripts/verify-release.py` refuses a release signed with anything else.

   ```bash
   keytool -exportcert -keystore uvcamera-release.jks -alias uvcamera \
       | sha256sum | cut -d' ' -f1 > release-signing-certificate.sha256
   ```

3. Add repository secrets for `.github/workflows/release.yml`:
   `RELEASE_KEYSTORE_BASE64` (`base64 -w0 uvcamera-release.jks`),
   `RELEASE_KEYSTORE_PASSPHRASE` and `RELEASE_KEY_PASSPHRASE`.

## Each release

1. Move the `Unreleased` entries in `CHANGELOG.md` under a heading for the
   new version, commit, then tag and push:

   ```bash
   git tag v0.1
   git push origin master v0.1
   ```

2. The release workflow builds and signs the module, checks it with
   `scripts/verify-release.py` (metadata, all ABIs, APK signature against the
   pin, the supervisor surviving R8), and stages a draft GitHub release with
   the zip, `SHA256SUMS` and the certificate pin. Review and publish it.

3. Point the update channel at it. Root managers read
   `app/module/updates/release/info.json` from `master` (the release
   module's `updateJson`), so only publish it after the release:

   ```bash
   git checkout v0.1
   ./gradlew updateJsonRelease
   git checkout master
   git add app/module/updates/release/info.json
   git commit -m "Update channel: v0.1"
   git push
   ```

## Local release build

With the key available, the same environment variables as CI:

```bash
RELEASE_KEYSTORE=/path/to/uvcamera-release.jks \
RELEASE_KEYSTORE_PASSPHRASE=... RELEASE_KEY_ALIAS=uvcamera RELEASE_KEY_PASSPHRASE=... \
    ./gradlew assembleRelease lintRelease zipRelease
python3 scripts/verify-release.py v0.1
```

Debug builds use the Android debug key and are not interchangeable with
release builds: installing one over the other needs an uninstall first.
