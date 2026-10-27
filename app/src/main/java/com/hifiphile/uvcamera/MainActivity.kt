// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera

import android.hardware.camera2.CameraManager
import android.hardware.camera2.CameraMetadata
import android.os.Build
import android.os.Bundle
import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.hifiphile.uvcamera.camera.CameraDetails
import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Facing
import com.hifiphile.uvcamera.camera.formatExposure
import com.hifiphile.uvcamera.camera.queryCameras
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.hifiphile.uvcamera.webcam.DaemonState
import com.hifiphile.uvcamera.webcam.WebcamSettingsCard
import com.hifiphile.uvcamera.webcam.WebcamStatusCard
import com.hifiphile.uvcamera.webcam.WebcamViewModel
import com.hifiphile.uvcamera.webcam.webcamModes

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)

        setContent {
            UVCameraTheme {
                MainScreen()
            }
        }
    }
}

@Composable
private fun UVCameraTheme(content: @Composable () -> Unit) {
    val context = LocalContext.current
    val dark = isSystemInDarkTheme()
    val colorScheme = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && dark -> dynamicDarkColorScheme(context)
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> dynamicLightColorScheme(context)
        dark -> darkColorScheme()
        else -> lightColorScheme()
    }

    MaterialTheme(colorScheme = colorScheme, content = content)
}

/** The app's pages, switched with the bottom navigation bar. */
private enum class Page(@StringRes val label: Int, @DrawableRes val icon: Int) {
    WEBCAM(R.string.tab_webcam, R.drawable.ic_notification),
    DEVICE(R.string.tab_device, R.drawable.ic_info),
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MainScreen() {
    var page by rememberSaveable { mutableStateOf(Page.WEBCAM) }
    BackHandler(enabled = page != Page.WEBCAM) { page = Page.WEBCAM }

    Scaffold(
        modifier = Modifier.fillMaxSize(),
        topBar = { TopAppBar(title = { Text(stringResource(R.string.cameras_title)) }) },
        bottomBar = {
            NavigationBar {
                for (p in Page.entries) {
                    NavigationBarItem(
                        selected = p == page,
                        onClick = { page = p },
                        icon = { Icon(painterResource(p.icon), contentDescription = null) },
                        label = { Text(stringResource(p.label)) },
                    )
                }
            }
        },
    ) { innerPadding ->
        val padding = PaddingValues(
            start = 16.dp,
            end = 16.dp,
            top = innerPadding.calculateTopPadding(),
            bottom = innerPadding.calculateBottomPadding() + 16.dp,
        )
        when (page) {
            Page.WEBCAM -> WebcamPage(padding)
            Page.DEVICE -> DevicePage(padding)
        }
    }
}

/** What the user needs to run the webcam: its state, on/off, and settings. */
@Composable
private fun WebcamPage(padding: PaddingValues) {
    LazyColumn(
        contentPadding = padding,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item { WebcamStatusCard() }
        item { WebcamSettingsCard() }
    }
}

/** The phone, the daemon and what each camera can stream. */
@Composable
private fun DevicePage(padding: PaddingValues) {
    val context = LocalContext.current
    val cameras = remember {
        runCatching { queryCameras(context.getSystemService(CameraManager::class.java)) }
    }

    LazyColumn(
        contentPadding = padding,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item { SystemCard() }

        item {
            Column(
                modifier = Modifier.padding(top = 8.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    stringResource(R.string.cameras_heading),
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    stringResource(R.string.cameras_subtitle),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        cameras.fold(
            onSuccess = { list ->
                if (list.isEmpty()) {
                    item { Text(stringResource(R.string.cameras_none)) }
                }
                items(list, key = { it.id }) { CameraCard(it) }
            },
            onFailure = { e ->
                item { Text(stringResource(R.string.cameras_error, e.toString())) }
            },
        )
    }
}

@Composable
private fun SystemCard(viewModel: WebcamViewModel = viewModel()) {
    val context = LocalContext.current
    val daemon by viewModel.daemon.collectAsStateWithLifecycle()
    val appVersion = remember {
        runCatching { context.packageManager.getPackageInfo(context.packageName, 0).versionName }
            .getOrNull() ?: "?"
    }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(stringResource(R.string.system_title), style = MaterialTheme.typography.titleMedium)
            InfoRow(stringResource(R.string.system_device), "${Build.MANUFACTURER} ${Build.MODEL}")
            InfoRow(
                stringResource(R.string.system_android),
                stringResource(R.string.system_android_version, Build.VERSION.RELEASE, Build.VERSION.SDK_INT),
            )
            InfoRow(stringResource(R.string.system_kernel), System.getProperty("os.version") ?: "?")
            InfoRow(stringResource(R.string.system_app), appVersion)
            InfoRow(
                stringResource(R.string.system_daemon),
                when (val state = daemon) {
                    DaemonState.Connecting -> stringResource(R.string.system_daemon_connecting)
                    is DaemonState.Connected -> stringResource(R.string.system_daemon_running)
                    is DaemonState.Unavailable ->
                        stringResource(R.string.system_daemon_unavailable, state.reason)
                },
            )
        }
    }
}

@Composable
private fun InfoRow(label: String, value: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(
            label,
            modifier = Modifier.width(80.dp),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(value, style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
private fun CameraCard(camera: CameraInfo) {
    val facing = stringResource(when (camera.facing) {
        Facing.BACK -> R.string.camera_facing_back
        Facing.FRONT -> R.string.camera_facing_front
        Facing.EXTERNAL -> R.string.camera_facing_external
    })
    val d = camera.details

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(
                stringResource(R.string.camera_title, camera.id, facing),
                style = MaterialTheme.typography.titleMedium,
            )
            InfoRow(
                stringResource(R.string.camera_webcam),
                webcamModes(camera).map { (size, fps) ->
                    if (fps != null) {
                        stringResource(R.string.camera_webcam_mode, size.height, fps)
                    } else {
                        stringResource(R.string.camera_webcam_padded, size.height)
                    }
                }.joinToString(" · "),
            )
            val mp = d.megapixels
            val array = d.activeArray
            if (mp != null && array != null) {
                InfoRow(
                    stringResource(R.string.camera_resolution),
                    d.maxMegapixels?.let { stringResource(R.string.camera_megapixels_full, mp, array.toString(), it) }
                        ?: stringResource(R.string.camera_megapixels, mp, array.toString()),
                )
            }
            val size = d.physicalSizeMm
            val format = d.opticalFormat
            val pixel = d.pixelSizeUm
            if (size != null && format != null && pixel != null) {
                InfoRow(
                    stringResource(R.string.camera_sensor),
                    stringResource(R.string.camera_sensor_value, format, size.first, size.second, pixel),
                )
            }
            val focal = d.focalLengthMm
            if (focal != null) {
                val aperture = d.apertures.minOrNull()
                val equivalent = d.equivalentFocalLengthMm
                InfoRow(
                    stringResource(R.string.camera_lens),
                    listOfNotNull(
                        stringResource(R.string.camera_focal_length, focal),
                        aperture?.let { stringResource(R.string.camera_aperture, it) },
                        equivalent?.let { stringResource(R.string.camera_equivalent, it) },
                    ).joinToString(" "),
                )
            }
            d.fieldOfView(16, 9)?.let { (h, v) ->
                InfoRow(stringResource(R.string.camera_view), stringResource(R.string.camera_view_value, h, v))
            }
            InfoRow(
                stringResource(R.string.camera_focus),
                d.minFocusCm?.takeIf { d.autofocus }
                    ?.let { stringResource(R.string.camera_focus_auto, it) }
                    ?: stringResource(R.string.camera_focus_fixed),
            )
            val iso = d.isoRange
            val exposure = d.exposureRangeNs
            if (iso != null && exposure != null) {
                InfoRow(
                    stringResource(R.string.camera_exposure),
                    stringResource(
                        R.string.camera_exposure_value,
                        iso.first,
                        iso.second,
                        formatExposure(exposure.first),
                        formatExposure(exposure.second),
                    ),
                )
            }
            camera.zoomRatioRange?.takeIf { (lo, hi) -> hi > lo }?.let { (lo, hi) ->
                InfoRow(stringResource(R.string.camera_zoom), stringResource(R.string.camera_zoom_value, lo, hi))
            }
            val features = cameraFeatures(d)
            if (features.isNotEmpty()) {
                InfoRow(stringResource(R.string.camera_features), features.map { stringResource(it) }.joinToString(", "))
            }
            if (d.physicalIds.isNotEmpty()) {
                InfoRow(stringResource(R.string.camera_combines), d.physicalIds.joinToString(", "))
            }
            InfoRow(stringResource(R.string.camera_level), camera.hardwareLevel)
        }
    }
}

/** Notable hardware and Camera2 features, as string resources. */
private fun cameraFeatures(d: CameraDetails): List<Int> = buildList {
    if (d.ois) add(R.string.camera_feature_ois)
    if (d.flash) add(R.string.camera_feature_flash)
    val caps = mapOf(
        CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_RAW to R.string.camera_feature_raw,
        CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR to R.string.camera_feature_manual,
        CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_DYNAMIC_RANGE_TEN_BIT to R.string.camera_feature_ten_bit,
        CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_CONSTRAINED_HIGH_SPEED_VIDEO to R.string.camera_feature_high_speed,
        CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_DEPTH_OUTPUT to R.string.camera_feature_depth,
        CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_MONOCHROME to R.string.camera_feature_monochrome,
    )
    for ((cap, label) in caps) {
        if (cap in d.capabilities) add(label)
    }
}
