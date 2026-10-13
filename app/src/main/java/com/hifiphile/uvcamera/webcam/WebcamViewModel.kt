// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

package com.hifiphile.uvcamera.webcam

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.hifiphile.uvcamera.daemon.Client
import com.hifiphile.uvcamera.daemon.FormatSpec
import com.hifiphile.uvcamera.daemon.Status
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

sealed interface DaemonState {
    data object Connecting : DaemonState

    /** uvcd isn't running, or refused us (not authorized, SELinux). */
    data class Unavailable(val reason: String) : DaemonState

    data class Connected(val status: Status) : DaemonState
}

private const val POLL_INTERVAL_MS = 1000L

class WebcamViewModel : ViewModel() {
    private val clientLock = Mutex()
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

    private val _busy = MutableStateFlow(false)
    val busy: StateFlow<Boolean> = _busy.asStateFlow()

    private val _actionError = MutableStateFlow<String?>(null)
    val actionError: StateFlow<String?> = _actionError.asStateFlow()

    /** Run [block] on a shared connection, reconnecting after failures. */
    private suspend fun <T> withClient(block: (Client) -> T): T = clientLock.withLock {
        val c = client ?: Client().also { client = it }
        try {
            block(c)
        } catch (e: Exception) {
            c.close()
            client = null
            throw e
        }
    }

    private suspend fun query(): DaemonState = try {
        DaemonState.Connected(withClient { it.getStatus() })
    } catch (e: Exception) {
        DaemonState.Unavailable(e.describe())
    }

    private fun act(block: (Client) -> Unit) {
        viewModelScope.launch {
            _busy.value = true
            _actionError.value = withContext(Dispatchers.IO) {
                try {
                    withClient(block)
                    null
                } catch (e: Exception) {
                    e.describe()
                }
            }
            _busy.value = false
        }
    }

    /** Enable the webcam. The host sees uvcd's test pattern until Phase 3. */
    fun enable() = act { it.enable(FormatSpec.defaults()) }

    fun disable() = act { it.disable() }

    override fun onCleared() {
        client?.close()
    }
}

private fun Throwable.describe(): String =
    generateSequence(this) { it.cause }.mapNotNull { it.message }.joinToString(": ")
