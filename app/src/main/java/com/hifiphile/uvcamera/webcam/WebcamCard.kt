// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import android.hardware.camera2.CameraManager
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.hifiphile.uvcamera.R
import com.hifiphile.uvcamera.camera.CameraInfo
import com.hifiphile.uvcamera.camera.Facing
import com.hifiphile.uvcamera.camera.Rotation
import com.hifiphile.uvcamera.camera.queryCameras
import com.hifiphile.uvcamera.daemon.FormatKind
import com.hifiphile.uvcamera.daemon.Status
import com.hifiphile.uvcamera.daemon.StreamParams
import java.util.Locale

private fun StreamParams.describe(): String {
    val kind = when (kind) {
        FormatKind.MJPEG -> "MJPEG"
        FormatKind.YUYV -> "YUYV"
    }
    return "$kind ${width}×$height @ ${String.format(Locale.ROOT, "%.0f", fps)} fps"
}

/** The webcam's state and the switch to turn it on or off. */
@Composable
fun WebcamStatusCard(viewModel: WebcamViewModel = viewModel()) {
    val context = LocalContext.current
    val daemon by viewModel.daemon.collectAsStateWithLifecycle()
    val service by WebcamService.state.collectAsStateWithLifecycle()
    var permissionDenied by remember { mutableStateOf(false) }

    val permissions = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { granted ->
        // Notifications are optional; the camera isn't.
        if (granted[Manifest.permission.CAMERA] == true) {
            permissionDenied = false
            WebcamService.start(context)
        } else {
            permissionDenied = true
        }
    }

    val status = (daemon as? DaemonState.Connected)?.status

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    stringResource(R.string.webcam_title),
                    modifier = Modifier.weight(1f),
                    style = MaterialTheme.typography.titleMedium,
                )
                if (service.running) {
                    OutlinedButton(onClick = { WebcamService.stop(context) }) {
                        Text(stringResource(R.string.webcam_disable))
                    }
                } else {
                    // Enabled without the app (test pattern, or the app was
                    // restarted): turning on attaches the camera to it.
                    if (status?.enabled == true) {
                        TextButton(onClick = { WebcamService.stop(context) }) {
                            Text(stringResource(R.string.webcam_disable))
                        }
                    }
                    Button(
                        onClick = {
                            val granted = ContextCompat.checkSelfPermission(
                                context, Manifest.permission.CAMERA,
                            ) == PackageManager.PERMISSION_GRANTED
                            if (granted) {
                                WebcamService.start(context)
                            } else {
                                permissions.launch(buildList {
                                    add(Manifest.permission.CAMERA)
                                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                                        add(Manifest.permission.POST_NOTIFICATIONS)
                                    }
                                }.toTypedArray())
                            }
                        },
                        enabled = status != null,
                    ) {
                        Text(stringResource(R.string.webcam_enable))
                    }
                }
            }

            when (val state = daemon) {
                DaemonState.Connecting -> Text(stringResource(R.string.webcam_connecting))
                is DaemonState.Unavailable -> {
                    Text(stringResource(R.string.webcam_unavailable))
                    Text(
                        state.reason,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                is DaemonState.Connected -> StatusRows(state.status, service)
            }

            if (permissionDenied) {
                Text(
                    stringResource(R.string.webcam_camera_permission),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            service.error?.let {
                Text(
                    stringResource(R.string.webcam_error, it),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
}

/** Camera, resolution and orientation, all applied live. */
@Composable
fun WebcamSettingsCard() {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                stringResource(R.string.webcam_settings_title),
                style = MaterialTheme.typography.titleMedium,
            )
            WebcamSettingsRows()
        }
    }
}

@Composable
private fun facingLabel(facing: Facing) = stringResource(when (facing) {
    Facing.BACK -> R.string.camera_facing_back
    Facing.FRONT -> R.string.camera_facing_front
    Facing.EXTERNAL -> R.string.camera_facing_external
})

/**
 * Camera, resolution cap and orientation, all applied live. A change that
 * alters the advertised formats reconnects the webcam on the host.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun WebcamSettingsRows() {
    val context = LocalContext.current
    val settings = remember { WebcamSettings(context) }
    val cameras = remember {
        runCatching { queryCameras(context.getSystemService(CameraManager::class.java)) }
            .getOrDefault(emptyList())
    }
    var cameraId by remember {
        mutableStateOf(WebcamSettings.pickCamera(cameras, settings.cameraId)?.id)
    }
    var maxResolution by remember { mutableStateOf(settings.maxResolution) }
    var rotation by remember { mutableStateOf(settings.rotation) }
    var mirrorH by remember { mutableStateOf(settings.mirrorH) }
    var mirrorV by remember { mutableStateOf(settings.mirrorV) }

    Text(
        stringResource(R.string.webcam_settings_camera),
        style = MaterialTheme.typography.labelLarge,
        modifier = Modifier.padding(top = 8.dp),
    )
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        for (camera: CameraInfo in cameras) {
            FilterChip(
                selected = camera.id == cameraId,
                onClick = {
                    cameraId = camera.id
                    settings.cameraId = camera.id
                },
                label = { Text(stringResource(R.string.webcam_camera_chip, camera.id, facingLabel(camera.facing))) },
            )
        }
    }

    Text(
        stringResource(R.string.webcam_settings_max_resolution),
        style = MaterialTheme.typography.labelLarge,
        modifier = Modifier.padding(top = 8.dp),
    )
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        for (resolution in MaxResolution.entries) {
            FilterChip(
                selected = resolution == maxResolution,
                onClick = {
                    maxResolution = resolution
                    settings.maxResolution = resolution
                },
                label = { Text(resolution.label) },
            )
        }
    }

    Text(
        stringResource(R.string.webcam_settings_orientation),
        style = MaterialTheme.typography.labelLarge,
        modifier = Modifier.padding(top = 8.dp),
    )
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        for (r in Rotation.entries) {
            FilterChip(
                selected = r == rotation,
                onClick = {
                    rotation = r
                    settings.rotation = r
                },
                label = { Text(stringResource(R.string.webcam_rotation_chip, r.degrees)) },
            )
        }
    }
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        FilterChip(
            selected = mirrorH,
            onClick = {
                mirrorH = !mirrorH
                settings.mirrorH = mirrorH
            },
            label = { Text(stringResource(R.string.webcam_mirror_h)) },
        )
        FilterChip(
            selected = mirrorV,
            onClick = {
                mirrorV = !mirrorV
                settings.mirrorV = mirrorV
            },
            label = { Text(stringResource(R.string.webcam_mirror_v)) },
        )
    }
    Text(
        stringResource(R.string.webcam_settings_orientation_note),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun StatusRows(status: Status, service: WebcamState) {
    if (!status.enabled) {
        Text(stringResource(R.string.webcam_disabled))
        return
    }

    Text(
        if (service.running) {
            stringResource(R.string.webcam_source_camera, service.camera ?: "?")
        } else {
            stringResource(R.string.webcam_source_test_pattern)
        }
    )
    Text(
        status.hostSpeed?.let { stringResource(R.string.webcam_host_connected, it.label) }
            ?: stringResource(R.string.webcam_host_waiting)
    )

    val stream = status.streaming
    if (stream == null) {
        Text(stringResource(R.string.webcam_idle))
    } else {
        Text(stringResource(R.string.webcam_streaming, stream.describe()))
        Text(
            pluralStringResource(
                R.plurals.webcam_stats,
                status.failed.toInt(),
                String.format(Locale.ROOT, "%.1f", status.fps),
                String.format(Locale.ROOT, "%.1f", status.sourceFps),
                status.failed,
            ),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
