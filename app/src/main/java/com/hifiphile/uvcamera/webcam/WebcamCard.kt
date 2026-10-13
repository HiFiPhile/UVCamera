// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.hifiphile.uvcamera.R
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

@Composable
fun WebcamCard(viewModel: WebcamViewModel = viewModel()) {
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
            Text(
                stringResource(R.string.webcam_title),
                style = MaterialTheme.typography.titleMedium,
            )

            when (val state = daemon) {
                DaemonState.Connecting -> Text(stringResource(R.string.webcam_connecting))
                is DaemonState.Unavailable -> {
                    Text(stringResource(R.string.webcam_unavailable))
                    Text(state.reason, style = MaterialTheme.typography.bodySmall)
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

            Row(
                modifier = Modifier.padding(top = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
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
                    enabled = status != null && !service.running,
                ) {
                    Text(stringResource(R.string.webcam_enable))
                }
                OutlinedButton(
                    onClick = { WebcamService.stop(context) },
                    enabled = service.running || status?.enabled == true,
                ) {
                    Text(stringResource(R.string.webcam_disable))
                }
            }
        }
    }
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
        )
    }
}
