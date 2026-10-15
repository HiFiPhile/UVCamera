// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera

import android.hardware.camera2.CameraManager
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
import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Facing
import com.hifiphile.uvcamera.camera.queryCameras
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.hifiphile.uvcamera.webcam.DaemonState
import com.hifiphile.uvcamera.webcam.WebcamSettingsCard
import com.hifiphile.uvcamera.webcam.WebcamStatusCard
import com.hifiphile.uvcamera.webcam.WebcamViewModel

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

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                stringResource(R.string.camera_title, camera.id, facing),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.camera_hardware_level, camera.hardwareLevel),
                style = MaterialTheme.typography.bodySmall,
            )
            for ((format, sizes) in camera.streamableSizes) {
                Text(
                    if (sizes.isEmpty()) {
                        stringResource(R.string.camera_format_none, format.label)
                    } else {
                        stringResource(
                            R.string.camera_format_sizes,
                            format.label,
                            sizes.joinToString(", "),
                        )
                    },
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
    }
}
