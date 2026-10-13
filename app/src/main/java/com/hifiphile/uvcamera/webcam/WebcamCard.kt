// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

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
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.hifiphile.uvcamera.R
import com.hifiphile.uvcamera.daemon.FormatKind
import com.hifiphile.uvcamera.daemon.Status
import java.util.Locale

@Composable
fun WebcamCard(viewModel: WebcamViewModel = viewModel()) {
    val daemon by viewModel.daemon.collectAsStateWithLifecycle()
    val busy by viewModel.busy.collectAsStateWithLifecycle()
    val actionError by viewModel.actionError.collectAsStateWithLifecycle()

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
                is DaemonState.Connected -> StatusRows(state.status)
            }

            actionError?.let {
                Text(
                    stringResource(R.string.webcam_action_failed, it),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            val status = (daemon as? DaemonState.Connected)?.status
            Row(
                modifier = Modifier.padding(top = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Button(
                    onClick = viewModel::enable,
                    enabled = status != null && !status.enabled && !busy,
                ) {
                    Text(stringResource(R.string.webcam_enable))
                }
                OutlinedButton(
                    onClick = viewModel::disable,
                    enabled = status != null && status.enabled && !busy,
                ) {
                    Text(stringResource(R.string.webcam_disable))
                }
            }
        }
    }
}

@Composable
private fun StatusRows(status: Status) {
    if (!status.enabled) {
        Text(stringResource(R.string.webcam_disabled))
        return
    }

    Text(stringResource(R.string.webcam_source_test_pattern))
    Text(
        status.hostSpeed?.let { stringResource(R.string.webcam_host_connected, it.label) }
            ?: stringResource(R.string.webcam_host_waiting)
    )

    val stream = status.streaming
    if (stream == null) {
        Text(stringResource(R.string.webcam_idle))
    } else {
        val kind = when (stream.kind) {
            FormatKind.MJPEG -> "MJPEG"
            FormatKind.YUYV -> "YUYV"
        }
        Text(
            stringResource(
                R.string.webcam_streaming,
                kind,
                stream.width,
                stream.height,
                String.format(Locale.ROOT, "%.0f", stream.fps),
            )
        )
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
