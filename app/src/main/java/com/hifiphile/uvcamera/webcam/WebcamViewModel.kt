// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.hifiphile.uvcamera.daemon.Client
import com.hifiphile.uvcamera.daemon.Status
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.stateIn

sealed interface DaemonState {
    data object Connecting : DaemonState

    /** uvcd isn't running, or refused us (not authorized, SELinux). */
    data class Unavailable(val reason: String) : DaemonState

    data class Connected(val status: Status) : DaemonState
}

private const val POLL_INTERVAL_MS = 1000L

class WebcamViewModel : ViewModel() {
    private var client: Client? = null

    /** Polled only while collected, i.e. while the screen is visible. */
    val daemon: StateFlow<DaemonState> = flow {
        while (true) {
            emit(query())
            delay(POLL_INTERVAL_MS)
        }
    }
        .flowOn(Dispatchers.IO)
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5000), DaemonState.Connecting)

    /** Called only from the polling flow, so no locking is needed. */
    private fun query(): DaemonState = try {
        val c = client ?: Client().also { client = it }
        DaemonState.Connected(c.getStatus())
    } catch (e: Exception) {
        client?.close()
        client = null
        DaemonState.Unavailable(e.describe())
    }

    override fun onCleared() {
        client?.close()
    }
}

fun Throwable.describe(): String =
    generateSequence(this) { it.cause }.mapNotNull { it.message }.joinToString(": ")
