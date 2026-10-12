// SPDX-FileCopyrightText: 2022-2026 Andrew Gunnerson
// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only
// Cargo and module packaging tasks are based on MSD.

import com.android.build.api.artifact.SingleArtifact
import com.android.build.api.variant.VariantOutputConfiguration
import org.gradle.kotlin.dsl.support.uppercaseFirstChar
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.compose.compiler)
}

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(21))
    }
}

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
        versionCode = 1
        versionName = "0.1.0"
    }
    buildTypes {
        getByName("release") {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
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
        inputs.property("rootProject.name", rootProject.name)
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

            outputFile.get().asFile.writeText(
                props.map { "${it.key}=${it.value}" }.joinToString("\n", postfix = "\n"))
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
        from(File(moduleDir, "service.sh"))

        from(File(rootDir, "LICENSE"))
        from(File(rootDir, "README.md"))
    }
}
