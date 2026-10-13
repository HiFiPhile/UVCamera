// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera

import android.hardware.camera2.CameraManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Facing
import com.hifiphile.uvcamera.camera.queryCameras
import com.hifiphile.uvcamera.webcam.WebcamCard

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)

        setContent {
            UVCameraTheme {
                CamerasScreen()
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

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CamerasScreen() {
    val context = LocalContext.current
    val cameras = remember {
        runCatching { queryCameras(context.getSystemService(CameraManager::class.java)) }
    }

    Scaffold(
        modifier = Modifier.fillMaxSize(),
        topBar = { TopAppBar(title = { Text(stringResource(R.string.cameras_title)) }) },
    ) { innerPadding ->
        LazyColumn(
            contentPadding = PaddingValues(
                start = 16.dp,
                end = 16.dp,
                top = innerPadding.calculateTopPadding(),
                bottom = innerPadding.calculateBottomPadding() + 16.dp,
            ),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item { WebcamCard() }

            item {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        stringResource(R.string.cameras_heading),
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(
                        stringResource(R.string.cameras_subtitle),
                        style = MaterialTheme.typography.bodyMedium,
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
