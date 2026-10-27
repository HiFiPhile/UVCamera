// SPDX-FileCopyrightText: 2022-2026 Andrew Gunnerson
// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only
// Cargo and module packaging tasks are based on MSD.

import com.android.build.api.artifact.SingleArtifact
import com.android.build.api.variant.VariantOutputConfiguration
import org.gradle.kotlin.dsl.support.uppercaseFirstChar
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import java.security.KeyStore
import java.security.MessageDigest

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.compose.compiler)
}

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(21))
    }
}

/**
 * Version from `git describe`: release tags are `v<major>.<minor>`; [count]
 * commits since the tag (or since the start, before the first tag).
 */
data class GitVersion(val major: Int, val minor: Int, val tag: String?, val count: Int, val hash: String) {
    // 8 bits each for major, minor and commits since the tag, as in MSD.
    val code: Int
        get() {
            require(major in 0..255 && minor in 0..255 && count in 0..255) { "Version out of range: $this" }
            return major.shl(16) or minor.shl(8) or count
        }

    val name: String
        get() = "$major.$minor" + if (count > 0) ".r$count.g$hash" else ""
}

fun git(vararg args: String): String = providers.exec {
    commandLine("git", *args)
    workingDir = rootDir
}.standardOutput.asText.get().trim()

val gitVersion = run {
    val describe = providers.exec {
        commandLine("git", "describe", "--long", "--tags", "--abbrev=7", "--match", "v[0-9]*")
        workingDir = rootDir
        isIgnoreExitValue = true
    }
    if (describe.result.get().exitValue == 0) {
        // v<major>.<minor>-<count>-g<hash>
        val match = Regex("""^(v(\d+)\.(\d+))-(\d+)-g([0-9a-f]+)$""")
            .matchEntire(describe.standardOutput.asText.get().trim())
            ?: throw GradleException("Tags must be v<major>.<minor>")
        val (tag, major, minor, count, hash) = match.destructured
        GitVersion(major.toInt(), minor.toInt(), tag, count.toInt(), hash)
    } else {
        GitVersion(0, 0, null, git("rev-list", "--count", "HEAD").toInt(), git("rev-parse", "--short=7", "HEAD"))
    }
}

val projectUrl = "https://github.com/HiFiPhile/UVCamera"
val releaseMetadataBranch = "master"

val extraDir = layout.buildDirectory.map { it.dir("extra") }

android {
    namespace = "com.hifiphile.uvcamera"

    compileSdk = 37
    buildToolsVersion = "37.0.0"
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "com.hifiphile.uvcamera"
        minSdk = 30
        targetSdk = 37
        versionCode = gitVersion.code
        versionName = gitVersion.name

        // The daemon is arm64 only, so the app's native code is too.
        ndk {
            abiFilters += "arm64-v8a"
        }
    }
    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = "3.31.6"
        }
    }
    signingConfigs {
        // From the environment, so the key never touches the repository.
        create("release") {
            val keystore = System.getenv("RELEASE_KEYSTORE")
            storeFile = keystore?.let(::File)
            storePassword = System.getenv("RELEASE_KEYSTORE_PASSPHRASE")
            keyAlias = System.getenv("RELEASE_KEY_ALIAS")
            keyPassword = System.getenv("RELEASE_KEY_PASSPHRASE")
        }
    }
    buildTypes {
        getByName("release") {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")

            signingConfig = signingConfigs.getByName("release")
        }
    }
    compileOptions {
        sourceCompatibility(JavaVersion.VERSION_21)
        targetCompatibility(JavaVersion.VERSION_21)
    }
    buildFeatures {
        buildConfig = true
        compose = true
    }
    dependenciesInfo {
        includeInApk = false
        includeInBundle = false
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_21
    }
}

dependencies {
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    debugImplementation(libs.androidx.compose.ui.tooling)
    testImplementation(libs.junit)
}

// Defer evaluation of a provider until the Exec task actually runs.
class LazyString(private val source: Lazy<String>) {
    constructor(source: Provider<String>) : this(lazy { source.get() })

    override fun toString() = source.value
}

val rustTargetMap = mapOf(
    "aarch64-linux-android" to "arm64-v8a",
    "thumbv7neon-linux-androideabi" to "armeabi-v7a",
    "x86_64-linux-android" to "x86_64",
)

val uvcdTasks = mutableMapOf<String, TaskProvider<Exec>>()

for ((target, abi) in rustTargetMap) {
    val suffix = abi.split('-', '_').joinToString("") { it.uppercaseFirstChar() }

    uvcdTasks[abi] = tasks.register<Exec>("uvcd$suffix") {
        val srcDir = File(rootDir, "uvcd")

        inputs.files(
            File(rootDir, "Cargo.toml"),
            File(rootDir, "Cargo.lock"),
            File(srcDir, "Cargo.toml"),
        )
        inputs.dir(File(srcDir, "src"))
        inputs.property("android.defaultConfig.minSdk", android.defaultConfig.minSdk!!)
        outputs.file(File(rootDir, "target/$target/release/uvcd"))

        executable = "cargo"
        args("android", "build", "--release", "--target", target)
        environment(
            "ANDROID_NDK_ROOT" to LazyString(androidComponents.sdkComponents.ndkDirectory
                .map { it.asFile.absolutePath }),
            "ANDROID_API" to android.defaultConfig.minSdk!!,
            "RUSTFLAGS" to "-C strip=symbols -C target-feature=+crt-static -C link-arg=-Wl,--allow-multiple-definition",
        )

        workingDir(srcDir)
    }
}

tasks.named<Delete>("clean") {
    for (target in rustTargetMap.keys) {
        delete.add(File(rootDir, "target/$target"))
    }
}

androidComponents.onVariants { variant ->
    val capitalized = variant.name.uppercaseFirstChar()
    val variantDir = extraDir.map { it.dir(variant.name) }
    val variantOutput = variant.outputs.first {
        it.outputType == VariantOutputConfiguration.OutputType.SINGLE
    }
    val variantVersionCode = variantOutput.versionCode
    val variantVersionName = variantOutput.versionName
    val variantApkFiles = variant.artifacts.get(SingleArtifact.APK).map {
        variant.artifacts.getBuiltArtifactsLoader().load(it)!!.elements.map { element ->
            element.outputFile
        }
    }

    val moduleProp = tasks.register("moduleProp$capitalized") {
        inputs.property("projectUrl", projectUrl)
        inputs.property("releaseMetadataBranch", releaseMetadataBranch)
        inputs.property("rootProject.name", rootProject.name)
        inputs.property("variant.name", variant.name)
        inputs.property("variant.applicationId", variant.applicationId)
        inputs.property("variantVersionCode", variantVersionCode)
        inputs.property("variantVersionName", variantVersionName)

        val outputFile = variantDir.map { it.file("module.prop") }
        outputs.file(outputFile)

        doLast {
            val props = linkedMapOf(
                "id" to variant.applicationId.get(),
                "name" to rootProject.name,
                "version" to "v${variantVersionName.get()}",
                "versionCode" to variantVersionCode.get().toString(),
                "author" to "HiFiPhile",
                "description" to "Use the phone as a USB webcam via the kernel UVC gadget",
            )
            // Root managers check this for module updates.
            if (variant.name == "release") {
                props["updateJson"] =
                    "$projectUrl/raw/$releaseMetadataBranch/app/module/updates/${variant.name}/info.json"
            }

            outputFile.get().asFile.writeText(
                props.map { "${it.key}=${it.value}" }.joinToString("\n", postfix = "\n"))
        }
    }

    // SHA-256 of the signing certificate. The module's root supervisor only
    // lets an app signed with this certificate talk to uvcd.
    val clientCertificate = tasks.register("clientCertificate$capitalized") {
        dependsOn("assemble$capitalized")
        val certificateFile = variantDir.map { it.file("client-cert.sha256") }
        outputs.file(certificateFile)
        // Always recheck the signing configuration rather than reusing a stale pin.
        outputs.upToDateWhen { false }
        doLast {
            val signing = android.buildTypes.getByName(variant.name).signingConfig
                ?.takeIf { it.storeFile != null }
                ?: throw GradleException(
                    "No signing key for ${variant.name}; set RELEASE_KEYSTORE and friends (docs/RELEASE.md)")
            val store = KeyStore.getInstance(signing.storeType ?: KeyStore.getDefaultType())
            signing.storeFile!!.inputStream().use {
                store.load(it, signing.storePassword!!.toCharArray())
            }
            val digest = MessageDigest.getInstance("SHA-256")
                .digest(store.getCertificate(signing.keyAlias!!).encoded)
            certificateFile.get().asFile.writeText(digest.joinToString("") {
                "%02x".format(it.toInt() and 0xff)
            } + "\n")
        }
    }

    tasks.register<Zip>("zip$capitalized") {
        inputs.property("rootProject.name", rootProject.name)
        inputs.property("variant.name", variant.name)
        inputs.property("variantVersionName", variantVersionName)
        inputs.files(variantApkFiles)

        archiveFileName.set("${rootProject.name}-${variantVersionName.get()}-${variant.name}.zip")
        // Force instantiation of old value or else this will cause infinite recursion
        destinationDirectory.set(destinationDirectory.dir(variant.name).get())

        // Make the zip byte-for-byte reproducible (note that the APK is still not reproducible)
        isPreserveFileTimestamps = false
        isReproducibleFileOrder = true

        from(moduleProp.map { it.outputs })
        from(clientCertificate.map { it.outputs })
        from(variantApkFiles) {
            rename { "${rootProject.name}.apk" }
        }
        for ((abi, task) in uvcdTasks) {
            from(task.map { it.outputs }) {
                rename { "uvcd.$abi" }
            }
        }

        val moduleDir = File(projectDir, "module")

        for (script in arrayOf("update-binary", "updater-script")) {
            from(File(moduleDir, script)) {
                into("META-INF/com/google/android")
            }
        }

        from(File(moduleDir, "customize.sh"))
        from(File(moduleDir, "post-fs-data.sh"))
        from(File(moduleDir, "service.sh"))

        from(File(rootDir, "LICENSE"))
        from(File(rootDir, "README.md"))
    }

    // The module update channel: run on the release tag, then commit the
    // result to the metadata branch once the release is published.
    tasks.register("updateJson$capitalized") {
        inputs.property("projectUrl", projectUrl)
        inputs.property("rootProject.name", rootProject.name)
        inputs.property("variant.name", variant.name)
        inputs.property("variantVersionCode", variantVersionCode)
        inputs.property("variantVersionName", variantVersionName)

        val updateDir = File(projectDir, "module/updates/${variant.name}")
        outputs.dir(updateDir)

        doLast {
            val tag = gitVersion.tag
            if (tag == null || gitVersion.count != 0) {
                throw GradleException("Check out the release tag first")
            }
            val zip = "${rootProject.name}-${variantVersionName.get()}-${variant.name}.zip"
            updateDir.mkdirs()
            File(updateDir, "info.json").writeText("""
                {
                    "version": "v${variantVersionName.get()}",
                    "versionCode": ${variantVersionCode.get()},
                    "zipUrl": "$projectUrl/releases/download/$tag/$zip",
                    "changelog": "$projectUrl/raw/$tag/CHANGELOG.md"
                }
            """.trimIndent() + "\n")
        }
    }
}
